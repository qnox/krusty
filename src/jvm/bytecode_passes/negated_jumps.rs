//! kotlinc's `NegatedJumpsMethodTransformer`: a conditional jump over a `goto`,
//! `if<cond> L; goto E; L:`, becomes the negated jump to the `goto`'s target, `if<!cond> E; L:`.
//!
//! kotlinc requires the `goto` to be the jump's next node and the jump's label the `goto`'s next
//! one. Its labels are normalized first, and ASM only creates a label something refers to, so a
//! label no jump, switch, line number, protected range or local variable names does not separate
//! the three here, and labels standing together count as one.
//!
//! A jump or `goto` to a `pinned` label is left alone (see `redundant_gotos`).

use std::collections::BTreeSet;

use crate::jvm::method_node::{Insn, LabelId, MethodNode, Node};

const GOTO: u8 = 0xa7;

fn negated(op: u8) -> Option<u8> {
    Some(match op {
        // `ifeq`/`ifne`, `iflt`/`ifge`, `ifgt`/`ifle`, `if_icmpeq`/`if_icmpne`, …: pairs of
        // consecutive opcodes starting at an odd one.
        0x99..=0xa6 if op % 2 == 1 => op + 1,
        0x99..=0xa6 => op - 1,
        0xc6 => 0xc7,
        0xc7 => 0xc6,
        _ => return None,
    })
}

/// Every label something in `method` refers to.
fn referenced(method: &MethodNode) -> BTreeSet<LabelId> {
    let mut labels = BTreeSet::new();
    for node in &method.nodes {
        match node {
            Node::Insn(insn) => labels.extend(insn.jump_targets()),
            Node::Line { start, .. } => {
                labels.insert(*start);
            }
            Node::Label(_) => {}
        }
    }
    for block in &method.try_catch_blocks {
        labels.extend([block.start, block.end, block.handler]);
    }
    for local in &method.local_variables {
        labels.extend([local.start, local.end]);
    }
    labels
}

/// The `goto`'s target and position when the conditional jump at node `at` is one to negate.
fn negatable(
    method: &MethodNode,
    at: usize,
    pinned: &BTreeSet<LabelId>,
) -> Option<(LabelId, usize)> {
    let nodes = &method.nodes;
    let Node::Insn(Insn::Jump { op, target: over }) = nodes[at] else {
        return None;
    };
    negated(op)?;
    if pinned.contains(&over) {
        return None;
    }
    let goto = at
        + 1
        + nodes[at + 1..]
            .iter()
            .position(|node| !matches!(node, Node::Label(_)))?;
    let Node::Insn(Insn::Jump {
        op: GOTO,
        target: to,
    }) = nodes[goto]
    else {
        return None;
    };
    if pinned.contains(&to) {
        return None;
    }
    if goto > at + 1 {
        let referenced = referenced(method);
        let separated = nodes[at + 1..goto]
            .iter()
            .any(|node| matches!(node, Node::Label(label) if referenced.contains(label)));
        if separated {
            return None;
        }
    }
    // The jump's own label stands right after the `goto`, before any instruction.
    let follows = nodes[goto + 1..]
        .iter()
        .take_while(|node| !matches!(node, Node::Insn(_)))
        .any(|node| *node == Node::Label(over));
    follows.then_some((to, goto))
}

/// Negate every conditional jump over a `goto` in `method`, leaving jumps to a `pinned` label
/// alone; `true` when anything changed.
pub(crate) fn negate(method: &mut MethodNode, pinned: &BTreeSet<LabelId>) -> bool {
    let mut changed = false;
    let mut at = 0;
    while at + 1 < method.nodes.len() {
        if let Some((to, goto)) = negatable(method, at, pinned) {
            let Node::Insn(Insn::Jump { op, .. }) = method.nodes[at] else {
                unreachable!("negatable returns only for a jump");
            };
            method.nodes[at] = Node::Insn(Insn::Jump {
                op: negated(op).expect("negatable checked the opcode"),
                target: to,
            });
            method.nodes.remove(goto);
            changed = true;
        }
        at += 1;
    }
    changed
}

#[cfg(test)]
mod tests {
    use super::*;

    const IFNULL: u8 = 0xc6;
    const IFNONNULL: u8 = 0xc7;
    const IFEQ: u8 = 0x99;

    /// `insns` with a label before each and one past the end; `Err((op, k))` jumps to label `k`.
    fn body(insns: &[Result<u8, (u8, usize)>]) -> (MethodNode, Vec<LabelId>) {
        let mut method = MethodNode::new(0x0009, "f", "(Ljava/lang/Object;)Ljava/lang/Object;");
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
        (method, labels)
    }

    fn null_check() -> (MethodNode, Vec<LabelId>) {
        // `…; dup; ifnull 3; goto 5; 3: pop; 4: aconst_null; 5: areturn`
        body(&[
            Ok(0x59),
            Err((IFNULL, 3)),
            Err((GOTO, 5)),
            Ok(0x57),
            Ok(0x01),
            Ok(0xb0),
        ])
    }

    #[test]
    fn a_null_check_over_a_goto_becomes_its_negation() {
        let (mut method, labels) = null_check();
        assert!(negate(&mut method, &BTreeSet::new()));
        let insns: Vec<Insn> = method.instructions().cloned().collect();
        assert_eq!(
            insns,
            vec![
                Insn::Op(0x59),
                Insn::Jump {
                    op: IFNONNULL,
                    target: labels[5],
                },
                Insn::Op(0x57),
                Insn::Op(0x01),
                Insn::Op(0xb0),
            ]
        );
    }

    #[test]
    fn a_line_number_on_the_goto_keeps_both() {
        let (mut method, labels) = null_check();
        let at = method
            .nodes
            .iter()
            .position(|node| *node == Node::Label(labels[2]))
            .expect("placed");
        method.nodes.insert(
            at + 1,
            Node::Line {
                line: 7,
                start: labels[2],
            },
        );
        assert!(!negate(&mut method, &BTreeSet::new()));
    }

    #[test]
    fn an_unreferenced_label_between_them_does_not_separate_them() {
        // The `goto`'s own label is named by nothing, as ASM would not have created it.
        let (mut method, _) = null_check();
        assert!(negate(&mut method, &BTreeSet::new()));
    }

    #[test]
    fn a_jump_over_a_goto_to_a_pinned_label_is_kept() {
        let (mut method, labels) = null_check();
        assert!(!negate(&mut method, &BTreeSet::from([labels[5]])));
        let (mut method, labels) = null_check();
        assert!(!negate(&mut method, &BTreeSet::from([labels[3]])));
    }

    #[test]
    fn a_jump_whose_target_is_not_right_after_the_goto_is_kept() {
        let (mut method, _) = body(&[
            Ok(0x1a),
            Err((IFEQ, 4)),
            Err((GOTO, 5)),
            Ok(0x04),
            Ok(0xac),
            Ok(0xb1),
        ]);
        assert!(!negate(&mut method, &BTreeSet::new()));
    }
}
