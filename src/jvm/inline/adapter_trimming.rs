//! Control-flow-safe removal of scalar adapters around an inlined lambda body.

use super::{BranchTarget, Insn};

/// Delete adjacent scalar adapters from a spliced lambda body while preserving its CFG.
///
/// Internal targets are instruction indices in the untrimmed body. A target at the body-end
/// boundary remains a target at the new end; an edge into an instruction being deleted makes the
/// cancellation unsafe and declines it. External targets are owned by the enclosing builder.
pub(super) fn trim(insns: &mut Vec<Insn>, prefix: usize, suffix: usize) -> Option<()> {
    let old_len = insns.len();
    let kept_end = old_len.checked_sub(suffix)?;
    if prefix > kept_end {
        return None;
    }
    let remap = |target: &mut usize| -> Option<()> {
        *target = if *target >= prefix && *target < kept_end {
            *target - prefix
        } else if *target == old_len {
            kept_end - prefix
        } else {
            return None;
        };
        Some(())
    };
    for instruction in insns.iter_mut() {
        match instruction {
            Insn::Branch { target, .. } | Insn::BranchW { target, .. } => {
                if let BranchTarget::Internal(target) = target {
                    remap(target)?;
                }
            }
            Insn::TableSwitch {
                default, targets, ..
            } => {
                remap(default)?;
                for target in targets {
                    remap(target)?;
                }
            }
            Insn::LookupSwitch { default, pairs } => {
                remap(default)?;
                for (_, target) in pairs {
                    remap(target)?;
                }
            }
            Insn::Plain { .. } => {}
        }
    }
    insns.truncate(kept_end);
    insns.drain(..prefix);
    Some(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remaps_internal_edges_and_the_end_boundary() {
        let mut body = vec![
            Insn::Plain {
                op: 0xc0,
                operands: vec![0, 1],
            },
            Insn::Branch {
                op: 0xa7,
                target: BranchTarget::Internal(3),
            },
            Insn::Plain {
                op: 0x03,
                operands: Vec::new(),
            },
            Insn::BranchW {
                op: 0xc8,
                target: BranchTarget::Internal(5),
            },
            Insn::Plain {
                op: 0xb8,
                operands: vec![0, 2],
            },
        ];

        trim(&mut body, 1, 1).expect("both adapters are outside the CFG");

        assert_eq!(body.len(), 3);
        assert!(matches!(
            body[0],
            Insn::Branch {
                target: BranchTarget::Internal(2),
                ..
            }
        ));
        assert!(matches!(
            body[2],
            Insn::BranchW {
                target: BranchTarget::Internal(3),
                ..
            }
        ));
    }

    #[test]
    fn rejects_an_edge_into_a_deleted_adapter() {
        let mut prefix_target = vec![
            Insn::Plain {
                op: 0xc0,
                operands: vec![0, 1],
            },
            Insn::Branch {
                op: 0xa7,
                target: BranchTarget::Internal(0),
            },
        ];
        assert_eq!(trim(&mut prefix_target, 1, 0), None);

        let mut suffix_target = vec![
            Insn::Branch {
                op: 0xa7,
                target: BranchTarget::Internal(1),
            },
            Insn::Plain {
                op: 0xb8,
                operands: vec![0, 2],
            },
        ];
        assert_eq!(trim(&mut suffix_target, 0, 1), None);
    }
}
