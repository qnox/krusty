//! kotlinc's final `DeadCodeEliminationMethodTransformer`, over a [`MethodNode`].
//!
//! After its optimization passes kotlinc always runs this transformer once more, so code a pass left
//! unreachable is gone from the class it writes: a `goto` whose jumps the redundant-`goto` pass
//! threaded past it, for one. `InstructionLivenessAnalyzer` walks the method from its first
//! instruction, following fall-through (everything but a `goto`, a switch, a return and `athrow`),
//! jump and switch targets, and, from every live instruction inside a protected range, that range's
//! handler. Every instruction it does not reach is removed.
//!
//! A line number is removed when, scanning forward past labels and past line numbers of the same
//! line, the scan reaches the end of the method or a different line having passed only dead
//! instructions; the first live instruction keeps it, and so does a different line reached with no
//! instruction in between. `removeEmptyCatchBlocks` then drops every protected range left with no
//! live instruction, and `prepareForEmitting` every local variable whose range holds none. Both
//! apply whether or not anything was dead now: an earlier pass (the constant-condition pass's own
//! dead-code step) can leave a range empty.
//!
//! The same transformer then renumbers the local slots (see [`super::local_slots`]).

use crate::jvm::method_node::LabelPositions;
use crate::jvm::method_node::{Insn, MethodNode, Node};

const JSR: u8 = 0xa8;
const RET: u8 = 0xa9;

/// What the removal took with the dead instructions.
#[derive(Debug, PartialEq)]
pub(crate) struct Elimination {
    /// Per local variable, in the order the method had them, whether its range holds no live
    /// instruction (it lost its last one, or an earlier pass emptied it) and the entry went.
    pub removed_locals: Vec<bool>,
}

/// A subroutine (`jsr`/`ret`), which kotlinc never emits and this analysis does not model.
fn is_subroutine(insn: &Insn) -> bool {
    matches!(insn, Insn::Jump { op: JSR, .. } | Insn::Var { op: RET, .. })
}

/// Per node, whether it is an instruction the method's entry reaches; per protected range, whether
/// a reached instruction stands inside it.
fn liveness(method: &MethodNode, at: &LabelPositions) -> (Vec<bool>, Vec<bool>) {
    let nodes = &method.nodes;
    let ranges: Vec<(usize, usize, usize)> = method
        .try_catch_blocks
        .iter()
        .map(|block| (at.at(block.start), at.at(block.end), at.at(block.handler)))
        .collect();
    let mut visited = vec![false; nodes.len()];
    let mut live = vec![false; nodes.len()];
    let mut handler_live = vec![false; ranges.len()];
    let mut pending = vec![0];
    while let Some(p) = pending.pop() {
        if p >= nodes.len() || visited[p] {
            continue;
        }
        visited[p] = true;
        let Node::Insn(insn) = &nodes[p] else {
            pending.push(p + 1);
            continue;
        };
        live[p] = true;
        if insn.falls_through() {
            pending.push(p + 1);
        }
        pending.extend(insn.jump_targets().into_iter().map(|label| at.at(label)));
        for (h, &(start, end, handler)) in ranges.iter().enumerate() {
            if (start..end).contains(&p) {
                handler_live[h] = true;
                pending.push(handler);
            }
        }
    }
    (live, handler_live)
}

/// Per node, whether it is a line number kotlinc's `shouldRemove` drops.
fn removed_lines(nodes: &[Node], live: &[bool]) -> Vec<bool> {
    let mut removed = vec![false; nodes.len()];
    for (at, node) in nodes.iter().enumerate() {
        let Node::Line { line, .. } = *node else {
            continue;
        };
        let mut passed_dead = false;
        removed[at] = 'scan: {
            for (p, node) in nodes.iter().enumerate().skip(at + 1) {
                match node {
                    Node::Line { line: other, .. } if *other == line => {}
                    Node::Line { .. } => break 'scan passed_dead,
                    Node::Insn(_) if live[p] => break 'scan false,
                    Node::Insn(_) => passed_dead = true,
                    Node::Label(_) => {}
                }
            }
            true
        };
    }
    removed
}

/// Remove every instruction the method's entry does not reach, with the line numbers, protected
/// ranges and local variables that go with them, and every protected range and local variable left
/// with no instruction; `None` when there is nothing to remove (or the body is outside what the
/// analysis models), leaving `method` as it was.
pub(crate) fn eliminate(method: &mut MethodNode) -> Option<Elimination> {
    if method.instructions().any(is_subroutine) {
        return None;
    }
    let at = LabelPositions::of(method);
    let (live, handler_live) = liveness(method, &at);
    let is_dead = |p: usize| matches!(method.nodes[p], Node::Insn(_)) && !live[p];
    // A local keeps its entry while a live instruction stands in its range, whether this pass or
    // an earlier one removed the others (`prepareForEmitting`).
    let removed_locals: Vec<bool> = method
        .local_variables
        .iter()
        .map(|local| !(at.at(local.start)..at.at(local.end)).any(|p| live[p]))
        .collect();
    if !(0..method.nodes.len()).any(is_dead)
        && handler_live.iter().all(|&kept| kept)
        && !removed_locals.contains(&true)
    {
        return None;
    }
    let removed_lines = removed_lines(&method.nodes, &live);

    let mut index = 0;
    method.nodes.retain(|node| {
        index += 1;
        let p = index - 1;
        match node {
            Node::Insn(_) => live[p],
            Node::Line { .. } => !removed_lines[p],
            Node::Label(_) => true,
        }
    });
    let mut block = 0;
    method.try_catch_blocks.retain(|_| {
        block += 1;
        handler_live[block - 1]
    });
    let mut local = 0;
    method.local_variables.retain(|_| {
        local += 1;
        !removed_locals[local - 1]
    });
    Some(Elimination { removed_locals })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::jvm::method_node::{LabelId, LocalVariable, TryCatchBlock};

    const IFEQ: u8 = 0x99;
    const IFNE: u8 = 0x9a;
    const GOTO: u8 = 0xa7;
    const IRETURN: u8 = 0xac;

    /// Builds a body from instructions and the labels, lines and ranges that refer into it, each
    /// placed before the instruction at the given index.
    struct Body {
        method: MethodNode,
        labels: Vec<LabelId>,
    }

    impl Body {
        /// `insns` with a label before each and one past the end; `jump(k)` targets label `k`.
        fn new(insns: &[Result<u8, (u8, usize)>]) -> Body {
            let mut method = MethodNode::new(0x0009, "f", "()I");
            let labels: Vec<LabelId> = (0..=insns.len()).map(|_| method.new_label()).collect();
            for (k, insn) in insns.iter().enumerate() {
                method.nodes.push(Node::Label(labels[k]));
                method.nodes.push(Node::Insn(match *insn {
                    Ok(op) => Insn::Op(op),
                    Err((op, to)) => Insn::Jump {
                        op,
                        target: labels[to],
                    },
                }));
            }
            method.nodes.push(Node::Label(labels[insns.len()]));
            Body { method, labels }
        }

        /// A line number at instruction `k`, after the line numbers already there.
        fn line(mut self, k: usize, line: u16) -> Body {
            let start = self.labels[k];
            let at = self
                .method
                .nodes
                .iter()
                .position(|node| *node == Node::Label(start))
                .expect("placed");
            let mut after = at + 1;
            while matches!(self.method.nodes.get(after), Some(Node::Line { .. })) {
                after += 1;
            }
            self.method.nodes.insert(after, Node::Line { line, start });
            self
        }

        fn instructions(&self) -> Vec<Insn> {
            self.method.instructions().cloned().collect()
        }

        fn lines(&self) -> Vec<u16> {
            self.method
                .nodes
                .iter()
                .filter_map(|node| match node {
                    Node::Line { line, .. } => Some(*line),
                    _ => None,
                })
                .collect()
        }
    }

    fn ops(ops: &[u8]) -> Vec<Insn> {
        ops.iter().map(|&op| Insn::Op(op)).collect()
    }

    /// `if (i == k) return 1` as a loop body's last statement, after its jump was threaded past the
    /// `goto` back to the loop head: 0 iload_0; 1 ifeq 5; 2 iconst_1; 3 ireturn; 4 goto 5;
    /// 5 iconst_0; 6 ireturn.
    fn threaded() -> Body {
        Body::new(&[
            Ok(0x1a),
            Err((IFEQ, 5)),
            Ok(0x04),
            Ok(IRETURN),
            Err((GOTO, 5)),
            Ok(0x03),
            Ok(IRETURN),
        ])
    }

    #[test]
    fn a_goto_nothing_reaches_is_removed_with_its_line() {
        let mut body = threaded().line(0, 1).line(4, 2).line(5, 3);
        let labels = body.labels.clone();
        eliminate(&mut body.method).expect("the goto is dead");
        let mut expected = ops(&[0x1a]);
        expected.push(Insn::Jump {
            op: IFEQ,
            target: labels[5],
        });
        expected.extend(ops(&[0x04, IRETURN, 0x03, IRETURN]));
        assert_eq!(body.instructions(), expected);
        assert_eq!(body.lines(), vec![1, 3]);
    }

    #[test]
    fn a_line_whose_scan_reaches_a_live_instruction_of_the_same_line_stays() {
        // The scan passes the dead `goto`, skips the next entry of the same line, and stops at the
        // live `iconst_0`.
        let mut body = threaded().line(4, 2).line(5, 2);
        eliminate(&mut body.method).expect("the goto is dead");
        assert_eq!(body.lines(), vec![2, 2]);
    }

    #[test]
    fn a_line_followed_directly_by_another_line_stays() {
        // 0 iconst_0; 1 ireturn; 2 goto 0: two lines stand at the dead `goto`. The first reaches the
        // second with no instruction between them, the second only dead code before the end.
        let mut body = Body::new(&[Ok(0x03), Ok(IRETURN), Err((GOTO, 0))])
            .line(0, 4)
            .line(2, 5)
            .line(2, 6);
        eliminate(&mut body.method).expect("the goto is dead");
        assert_eq!(body.instructions(), ops(&[0x03, IRETURN]));
        assert_eq!(body.lines(), vec![4, 5]);
    }

    #[test]
    fn a_protected_range_left_with_only_dead_code_goes_with_its_handler() {
        // 0 iconst_0; 1 ireturn; 2 nop (protected, dead); 3 astore_0 (handler); 4 iconst_1;
        // 5 ireturn.
        let mut body = Body::new(&[
            Ok(0x03),
            Ok(IRETURN),
            Ok(0x00),
            Ok(0x4b),
            Ok(0x04),
            Ok(IRETURN),
        ]);
        let labels = body.labels.clone();
        body.method.try_catch_blocks.push(TryCatchBlock {
            start: labels[2],
            end: labels[3],
            handler: labels[3],
            catch_type: None,
        });
        for (start, end) in [(3, 6), (0, 6)] {
            body.method.local_variables.push(LocalVariable {
                name: "x".to_string(),
                desc: "I".to_string(),
                start: labels[start],
                end: labels[end],
                slot: 0,
            });
        }
        let elimination = eliminate(&mut body.method).expect("the range and its handler are dead");
        assert_eq!(body.instructions(), ops(&[0x03, IRETURN]));
        assert!(body.method.try_catch_blocks.is_empty());
        assert_eq!(elimination.removed_locals, vec![true, false]);
        assert_eq!(body.method.local_variables.len(), 1);
    }

    #[test]
    fn a_live_protected_range_keeps_its_handler_live() {
        // 0 iconst_0; 1 ireturn (protected); 2 astore_0 (handler); 3 iconst_1; 4 ireturn;
        // 5 goto 0 (dead).
        let mut body = Body::new(&[
            Ok(0x03),
            Ok(IRETURN),
            Ok(0x4b),
            Ok(0x04),
            Ok(IRETURN),
            Err((GOTO, 0)),
        ]);
        let labels = body.labels.clone();
        body.method.try_catch_blocks.push(TryCatchBlock {
            start: labels[0],
            end: labels[2],
            handler: labels[2],
            catch_type: None,
        });
        eliminate(&mut body.method).expect("the trailing goto is dead");
        assert_eq!(
            body.instructions(),
            ops(&[0x03, IRETURN, 0x4b, 0x04, IRETURN])
        );
        assert_eq!(body.method.try_catch_blocks.len(), 1);
    }

    #[test]
    fn a_method_without_dead_code_is_left_alone() {
        let mut body = Body::new(&[Ok(0x1a), Err((IFEQ, 3)), Ok(0x04), Ok(0x03), Ok(IRETURN)]);
        let before = body.method.clone();
        assert_eq!(eliminate(&mut body.method), None);
        assert_eq!(body.method, before);
    }

    #[test]
    fn a_range_an_earlier_pass_emptied_goes_though_nothing_is_dead() {
        // 0 iconst_0; 1 ireturn, with a protected range and a local over the empty span before
        // instruction 0: kotlinc's `removeEmptyCatchBlocks` and `prepareForEmitting` drop both.
        let mut body = Body::new(&[Ok(0x03), Ok(IRETURN)]);
        let labels = body.labels.clone();
        let empty = body.method.new_label();
        body.method.nodes.insert(1, Node::Label(empty));
        body.method.try_catch_blocks.push(TryCatchBlock {
            start: labels[0],
            end: empty,
            handler: labels[1],
            catch_type: None,
        });
        for (start, end) in [(labels[0], empty), (labels[0], labels[2])] {
            body.method.local_variables.push(LocalVariable {
                name: "x".to_string(),
                desc: "I".to_string(),
                start,
                end,
                slot: 0,
            });
        }
        let elimination = eliminate(&mut body.method).expect("the empty range and local go");
        assert_eq!(body.instructions(), ops(&[0x03, IRETURN]));
        assert!(body.method.try_catch_blocks.is_empty());
        assert_eq!(elimination.removed_locals, vec![true, false]);
        assert_eq!(body.method.local_variables.len(), 1);
    }

    #[test]
    fn a_jump_reaches_the_instruction_after_its_label() {
        // 0 iload_0; 1 ifne 3; 2 ireturn; 3 iconst_0; 4 ireturn: every instruction is live.
        let mut body = Body::new(&[Ok(0x1a), Err((IFNE, 3)), Ok(IRETURN), Ok(0x03), Ok(IRETURN)]);
        assert_eq!(eliminate(&mut body.method), None);
    }
}
