//! kotlinc's `RedundantGotoMethodTransformer` and the `nop` cleanup that follows it
//! (`RedundantNopsCleanupMethodTransformer`), over a [`MethodNode`].
//!
//! The first walks the instruction list backwards. Labels and `nop`s are transparent; a line number
//! or any other instruction ends the run of labels in front of it. A `goto` whose target is one of
//! the labels right after it does nothing and becomes a `nop`; and every jump to a label that leads
//! straight into a `goto` is threaded to where that `goto` (and any `goto` it leads into in turn)
//! finally goes.
//!
//! The cleanup then removes those `nop`s, except the first `nop` of a debug range (from a line
//! number or a local variable's range boundary up to the next line number) that holds no other
//! instruction, and a `nop` that is the first instruction of a protected range. Only the `nop`s this
//! pass made are cleaned up: every other `nop` krusty writes is already one kotlinc keeps.
//!
//! Jumps to a `pinned` label are left alone: a `goto` to one is neither redundant nor threaded
//! through, and a jump to one is not threaded. krusty pins the labels that stand after an
//! instruction its null-check rules inserted in front of the jump's original target, where kotlinc
//! would have a line number between them.

use std::collections::{BTreeSet, HashMap};

use crate::jvm::method_node::{Insn, LabelId, MethodNode, Node};

const GOTO: u8 = 0xa7;
const NOP: u8 = 0x00;

fn goto_target(node: &Node) -> Option<LabelId> {
    match node {
        Node::Insn(Insn::Jump { op: GOTO, target }) => Some(*target),
        _ => None,
    }
}

/// Where a jump to `label` finally goes through the `goto`s `leads_to` names; `None` on a cycle.
/// Results, cycles included, are cached for every label visited, so a long chain is walked once
/// rather than once per jump into it.
fn final_target(
    label: LabelId,
    leads_to: &HashMap<LabelId, LabelId>,
    resolved: &mut HashMap<LabelId, Option<LabelId>>,
) -> Option<LabelId> {
    let mut path = Vec::new();
    let mut seen = BTreeSet::new();
    let mut at = label;
    let result = loop {
        if let Some(result) = resolved.get(&at) {
            break *result;
        }
        if !seen.insert(at) {
            break None;
        }
        let Some(&next) = leads_to.get(&at) else {
            break Some(at);
        };
        path.push(at);
        at = next;
    };
    for label in path {
        resolved.insert(label, result);
    }
    result
}

fn is_nop(node: &Node) -> bool {
    matches!(node, Node::Insn(Insn::Op(NOP)))
}

/// Remove kotlinc's redundant `goto`s from `method` and thread jumps through `goto`s, leaving jumps
/// to a `pinned` label alone; `true` when anything changed.
pub(crate) fn remove(method: &mut MethodNode, pinned: &BTreeSet<LabelId>) -> bool {
    // Backwards: `run` holds the labels standing in front of the next instruction that is not a
    // `nop`; `pending` is that instruction's target when it is a `goto`.
    let mut run: BTreeSet<LabelId> = BTreeSet::new();
    let mut pending: Option<LabelId> = None;
    let mut leads_to: HashMap<LabelId, LabelId> = HashMap::new();
    let mut redundant: Vec<usize> = Vec::new();
    for (at, node) in method.nodes.iter().enumerate().rev() {
        match node {
            Node::Label(label) => {
                run.insert(*label);
                if let Some(to) = pending {
                    leads_to.insert(*label, to);
                }
            }
            _ if goto_target(node).is_some_and(|target| !pinned.contains(&target)) => {
                let target = goto_target(node).expect("matched a goto");
                pending = Some(target);
                if run.contains(&target) {
                    redundant.push(at);
                } else {
                    run.clear();
                }
            }
            _ if is_nop(node) => {}
            Node::Line { .. } | Node::Insn(_) => {
                run.clear();
                pending = None;
            }
        }
    }

    let mut changed = false;
    // Thread every jump through the `goto`s its target leads into.
    let mut resolved = HashMap::new();
    for node in &mut method.nodes {
        if let Node::Insn(Insn::Jump { target, .. }) = node {
            if pinned.contains(target) {
                continue;
            }
            if let Some(last) = final_target(*target, &leads_to, &mut resolved) {
                if last != *target {
                    *target = last;
                    changed = true;
                }
            }
        }
    }
    if redundant.is_empty() {
        return changed;
    }
    for &at in &redundant {
        method.nodes[at] = Node::Insn(Insn::Op(NOP));
    }
    let required = required_nops(method);
    let mut removed: Vec<usize> = redundant
        .into_iter()
        .filter(|at| !required.contains(at))
        .collect();
    removed.sort_unstable();
    for &at in removed.iter().rev() {
        method.nodes.remove(at);
    }
    true
}

/// The `nop`s kotlinc's cleanup keeps, by node position.
pub(crate) fn required_nops(method: &MethodNode) -> BTreeSet<usize> {
    let nodes = &method.nodes;
    let position = |label: LabelId| {
        nodes
            .iter()
            .position(|node| *node == Node::Label(label))
            .expect("a label the method names stands in it")
    };
    let mut required = BTreeSet::new();
    for block in &method.try_catch_blocks {
        let first =
            (position(block.start)..nodes.len()).find(|&at| matches!(nodes[at], Node::Insn(_)));
        if let Some(at) = first.filter(|&at| is_nop(&nodes[at])) {
            required.insert(at);
        }
    }
    // Debug points in list order: a local variable's range boundary, a line number.
    let bounds: BTreeSet<LabelId> = method
        .local_variables
        .iter()
        .flat_map(|local| [local.start, local.end])
        .collect();
    let points: Vec<(usize, bool)> = nodes
        .iter()
        .enumerate()
        .filter_map(|(at, node)| match node {
            Node::Label(label) if bounds.contains(label) => Some((at, false)),
            Node::Line { .. } => Some((at, true)),
            _ => None,
        })
        .collect();
    for pair in points.windows(2) {
        let [(from, _), (to, true)] = *pair else {
            continue;
        };
        let mut first_nop = None;
        let mut only_nops = true;
        for (at, node) in nodes.iter().enumerate().take(to).skip(from) {
            match node {
                _ if is_nop(node) => {
                    first_nop.get_or_insert(at);
                }
                Node::Insn(_) => {
                    only_nops = false;
                    break;
                }
                _ => {}
            }
        }
        if let (true, Some(at)) = (only_nops, first_nop) {
            required.insert(at);
        }
    }
    required
}

#[cfg(test)]
mod tests {
    use super::*;

    const IFEQ: u8 = 0x99;
    const IFNONNULL: u8 = 0xc7;
    const ARETURN: u8 = 0xb0;
    const IRETURN: u8 = 0xac;

    /// A body from instructions with a label before each and one past the end; `Err((op, k))` jumps
    /// to label `k`. `lines` start at the given instructions; `bounds` are one local's range.
    struct Body {
        method: MethodNode,
        labels: Vec<LabelId>,
    }

    fn body(
        insns: &[Result<u8, (u8, usize)>],
        lines: &[usize],
        bounds: Option<(usize, usize)>,
    ) -> Body {
        let mut method = MethodNode::new(0x0009, "f", "()I");
        let labels: Vec<LabelId> = (0..=insns.len()).map(|_| method.new_label()).collect();
        for (k, insn) in insns.iter().enumerate() {
            method.nodes.push(Node::Label(labels[k]));
            if lines.contains(&k) {
                method.nodes.push(Node::Line {
                    line: k as u16 + 1,
                    start: labels[k],
                });
            }
            method.nodes.push(Node::Insn(match *insn {
                Ok(op) => Insn::Op(op),
                Err((op, to)) => Insn::Jump {
                    op,
                    target: labels[to],
                },
            }));
        }
        method.nodes.push(Node::Label(labels[insns.len()]));
        if let Some((start, end)) = bounds {
            method
                .local_variables
                .push(crate::jvm::method_node::LocalVariable {
                    name: "x".to_string(),
                    desc: "I".to_string(),
                    start: labels[start],
                    end: labels[end],
                    slot: 0,
                });
        }
        Body { method, labels }
    }

    impl Body {
        fn run(self) -> Option<Vec<Insn>> {
            self.run_pinned(&[])
        }

        fn run_pinned(mut self, pinned: &[usize]) -> Option<Vec<Insn>> {
            let pinned = pinned.iter().map(|&k| self.labels[k]).collect();
            remove(&mut self.method, &pinned).then(|| self.method.instructions().cloned().collect())
        }

        fn jump(&self, op: u8, to: usize) -> Insn {
            Insn::Jump {
                op,
                target: self.labels[to],
            }
        }
    }

    fn ops(ops: &[u8]) -> Vec<Insn> {
        ops.iter().map(|&op| Insn::Op(op)).collect()
    }

    #[test]
    fn a_goto_to_the_next_instruction_is_removed() {
        // `s ?: "d"` after the null-check fold: 0 aload_0; 1 dup; 2 ifnonnull 6; 3 pop;
        // 4 aconst_null; 5 goto 6; 6 areturn.
        let body = body(
            &[
                Ok(0x2a),
                Ok(0x59),
                Err((IFNONNULL, 6)),
                Ok(0x57),
                Ok(0x01),
                Err((GOTO, 6)),
                Ok(ARETURN),
            ],
            &[0],
            Some((0, 7)),
        );
        let mut expected = ops(&[0x2a, 0x59]);
        expected.push(body.jump(IFNONNULL, 6));
        expected.extend(ops(&[0x57, 0x01, ARETURN]));
        assert_eq!(body.run(), Some(expected));
    }

    #[test]
    fn a_line_number_at_a_label_between_keeps_the_goto() {
        // 0 iconst_0; 1 goto 3; 2 (a line starts here) nop; 3 ireturn: the line between ends the
        // run.
        let body = body(
            &[Ok(0x03), Err((GOTO, 3)), Ok(NOP), Ok(IRETURN)],
            &[0, 2],
            None,
        );
        assert_eq!(body.run(), None);
    }

    #[test]
    fn a_goto_alone_on_its_line_leaves_a_nop() {
        // 0 iconst_0; 1 istore_0; 2 (line) goto 3; 3 (line) iload_0; 4 ireturn: the goto is the
        // only instruction of its line, so kotlinc keeps a `nop` there.
        let body = body(
            &[Ok(0x03), Ok(0x3b), Err((GOTO, 3)), Ok(0x1a), Ok(IRETURN)],
            &[0, 2, 3],
            None,
        );
        assert_eq!(body.run(), Some(ops(&[0x03, 0x3b, NOP, 0x1a, IRETURN])));
    }

    #[test]
    fn a_jump_into_a_goto_goes_where_the_goto_goes() {
        // 0 iload_0; 1 ifeq 4; 2 iconst_1; 3 ireturn; 4 goto 6; 5 iconst_2; 6 iconst_0; 7 ireturn.
        let body = body(
            &[
                Ok(0x1a),
                Err((IFEQ, 4)),
                Ok(0x04),
                Ok(IRETURN),
                Err((GOTO, 6)),
                Ok(0x05),
                Ok(0x03),
                Ok(IRETURN),
            ],
            &[],
            None,
        );
        let mut expected = ops(&[0x1a]);
        expected.push(body.jump(IFEQ, 6));
        expected.extend(ops(&[0x04, IRETURN]));
        expected.push(body.jump(GOTO, 6));
        expected.extend(ops(&[0x05, 0x03, IRETURN]));
        assert_eq!(body.run(), Some(expected));
    }

    #[test]
    fn a_goto_to_a_pinned_label_is_neither_threaded_through_nor_removed() {
        // The jump at 1 would go through the goto at 4 to 6, and the goto at 5 is redundant; both
        // target a pinned label.
        let body = body(
            &[
                Ok(0x1a),
                Err((IFEQ, 4)),
                Ok(0x04),
                Ok(IRETURN),
                Err((GOTO, 6)),
                Err((GOTO, 6)),
                Ok(0x03),
                Ok(IRETURN),
            ],
            &[],
            None,
        );
        assert_eq!(body.run_pinned(&[6]), None);
    }

    #[test]
    fn a_nop_that_starts_a_protected_range_stays() {
        // 0 iconst_0; 1 goto 2; 2 ireturn, with a range protecting only the goto.
        let mut body = body(&[Ok(0x03), Err((GOTO, 2)), Ok(IRETURN)], &[], None);
        let labels = body.labels.clone();
        body.method
            .try_catch_blocks
            .push(crate::jvm::method_node::TryCatchBlock {
                start: labels[1],
                end: labels[2],
                handler: labels[2],
                catch_type: None,
            });
        assert_eq!(body.run(), Some(ops(&[0x03, NOP, IRETURN])));
    }

    #[test]
    fn a_long_goto_chain_is_resolved_once_and_removed() {
        let links = 4_096;
        let end = links + 1;
        let mut insns = vec![Err((IFEQ, 1))];
        insns.extend((1..=links).map(|at| Err((GOTO, at + 1))));
        insns.push(Ok(ARETURN));
        let body = body(&insns, &[], None);
        let expected = vec![body.jump(IFEQ, end), Insn::Op(ARETURN)];
        assert_eq!(body.run(), Some(expected));
    }
}
