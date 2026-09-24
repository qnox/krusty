//! kotlinc's `NegatedJumpsMethodTransformer`, the last of its bytecode passes: a conditional jump
//! over a `goto` — `if<cond> L; goto E; L:` — becomes the negated jump to the `goto`'s target,
//! `if<!cond> E; L:`. The `goto` must follow the jump directly, and `L` must stand right after the
//! `goto`; labels bound together count as one, since kotlinc normalizes them before this pass.

use super::temporaries::Placement;
use crate::jvm::inline::{BranchTarget, Insn};

const GOTO: u8 = 0xa7;

fn negated(op: u8) -> Option<u8> {
    Some(match op {
        0x99..=0xa6 => {
            // `ifeq`/`ifne`, `iflt`/`ifge`, `ifgt`/`ifle`, `if_icmpeq`/`if_icmpne`, …: pairs of
            // consecutive opcodes starting at an odd one.
            if op % 2 == 1 {
                op + 1
            } else {
                op - 1
            }
        }
        0xc6 => 0xc7,
        0xc7 => 0xc6,
        _ => return None,
    })
}

/// Negate every conditional jump over a `goto` in `nodes`; `true` when anything changed.
///
/// `labelled` says whether a label other than a branch target stands at an original index (a line
/// number, a local variable's range boundary, a protected range's bound); `late_branch` whether the
/// branch at an original index jumps past an instruction the null-check rules inserted at its
/// target, which this pass leaves alone.
pub(crate) fn negate(
    nodes: &mut Vec<(Insn, Placement)>,
    labelled: &[bool],
    late_branch: &dyn Fn(usize) -> bool,
) -> bool {
    let mut changed = false;
    let mut at = 0;
    while at + 2 <= nodes.len() {
        if let Some(to) = negatable(nodes, at, labelled, late_branch) {
            let Insn::Branch { op, .. } = nodes[at].0 else {
                unreachable!("negatable returns only for a two-byte branch");
            };
            nodes[at].0 = Insn::Branch {
                op: negated(op).expect("negatable checked the opcode"),
                target: BranchTarget::Internal(to),
            };
            nodes.remove(at + 1);
            changed = true;
        }
        at += 1;
    }
    changed
}

/// The `goto`'s target when the conditional jump at `at` is one to negate.
fn negatable(
    nodes: &[(Insn, Placement)],
    at: usize,
    labelled: &[bool],
    late_branch: &dyn Fn(usize) -> bool,
) -> Option<usize> {
    let late = |placement: Placement| matches!(placement, Placement::Original(index) if late_branch(index));
    let Insn::Branch {
        op,
        target: BranchTarget::Internal(over),
    } = nodes[at].0
    else {
        return None;
    };
    negated(op)?;
    let Insn::Branch {
        op: GOTO,
        target: BranchTarget::Internal(to),
    } = nodes.get(at + 1)?.0
    else {
        return None;
    };
    if late(nodes[at].1) || late(nodes[at + 1].1) {
        return None;
    }
    // Nothing between the jump and the `goto`: no label of any group the `goto` begins.
    let (jump_group, goto_group) = (nodes[at].1.group(), nodes[at + 1].1.group());
    if goto_group != jump_group
        && (jump_group + 1..=goto_group)
            .any(|group| labelled.get(group).copied().unwrap_or(false) || targeted(nodes, group))
    {
        return None;
    }
    // The jump's own label stands right after the `goto`: the next instruction is the first one
    // at or after the jump's target, with only labels between.
    let next = nodes
        .iter()
        .position(|(_, placement)| placement.group() >= over)
        .unwrap_or(nodes.len());
    (next == at + 2 && over > goto_group).then_some(to)
}

fn targeted(nodes: &[(Insn, Placement)], group: usize) -> bool {
    nodes.iter().any(|(insn, _)| match insn {
        Insn::Branch {
            target: BranchTarget::Internal(to),
            ..
        }
        | Insn::BranchW {
            target: BranchTarget::Internal(to),
            ..
        } => *to == group,
        Insn::TableSwitch {
            default, targets, ..
        } => *default == group || targets.contains(&group),
        Insn::LookupSwitch { default, pairs } => {
            *default == group || pairs.iter().any(|&(_, to)| to == group)
        }
        _ => false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn op(op: u8) -> Insn {
        Insn::Plain {
            op,
            operands: Vec::new(),
        }
    }

    fn branch(op: u8, to: usize) -> Insn {
        Insn::Branch {
            op,
            target: BranchTarget::Internal(to),
        }
    }

    fn run(insns: &[Insn], labelled: &[usize]) -> Option<Vec<Insn>> {
        let mut nodes: Vec<(Insn, Placement)> = insns
            .iter()
            .enumerate()
            .map(|(index, insn)| (insn.clone(), Placement::Original(index)))
            .collect();
        let mut marks = vec![false; insns.len() + 1];
        for &index in labelled {
            marks[index] = true;
        }
        negate(&mut nodes, &marks, &|_| false)
            .then(|| nodes.into_iter().map(|(insn, _)| insn).collect())
    }

    #[test]
    fn a_null_check_over_a_goto_becomes_its_negation() {
        // `…; dup; ifnull 4; goto 6; 4: pop; 5: ldc; 6: areturn` → `ifnonnull 6; 4: pop; …`
        let insns = [
            op(0x59),
            branch(0xc6, 3),
            branch(GOTO, 5),
            op(0x57),
            op(0x12),
            op(0xb0),
        ];
        assert_eq!(
            run(&insns, &[]),
            Some(vec![
                op(0x59),
                branch(0xc7, 5),
                op(0x57),
                op(0x12),
                op(0xb0)
            ])
        );
    }

    #[test]
    fn a_line_number_on_the_goto_keeps_both() {
        let insns = [
            op(0x59),
            branch(0xc6, 3),
            branch(GOTO, 5),
            op(0x57),
            op(0x12),
            op(0xb0),
        ];
        assert_eq!(run(&insns, &[2]), None);
    }

    #[test]
    fn a_jump_whose_target_is_not_right_after_the_goto_is_kept() {
        let insns = [
            op(0x1a),
            branch(0x99, 4),
            branch(GOTO, 5),
            op(0x04),
            op(0xac),
            op(0xb1),
        ];
        assert_eq!(run(&insns, &[]), None);
    }
}
