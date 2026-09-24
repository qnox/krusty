use super::{BranchTarget, Insn};

/// Whether the continuation immediately after a transformed splice is reachable from its entry.
/// Returns have already become either a fallthrough at the end boundary or a branch to it, and
/// literal-lambda substitutions are already present. Thus a return opcode remaining in the graph is
/// a non-local lambda return and terminates rather than reaching the caller continuation.
pub(super) fn caller_continuation_reachable(
    insns: &[Insn],
    offsets: &[usize],
    handlers: &[(usize, usize, usize, u16)],
) -> Option<bool> {
    let mut pending = vec![0usize];
    let mut visited = vec![false; insns.len()];
    while let Some(index) = pending.pop() {
        if index == insns.len() {
            return Some(true);
        }
        if index > insns.len() || std::mem::replace(&mut visited[index], true) {
            continue;
        }
        let byte_offset = *offsets.get(index)?;
        for &(start, end, handler, _) in handlers {
            if start <= byte_offset && byte_offset < end {
                pending.push(offsets.binary_search(&handler).ok()?);
            }
        }
        let next = index + 1;
        match &insns[index] {
            Insn::Plain {
                op: 0xac..=0xb1 | 0xbf,
                ..
            } => {}
            Insn::Plain { op: 0xa9, .. } => return None,
            Insn::Plain { op: 0xc4, operands } if operands.first() == Some(&0xa9) => return None,
            Insn::Plain { .. } => pending.push(next),
            Insn::Branch { op: 0xa7, target } | Insn::BranchW { op: 0xc8, target } => {
                if let BranchTarget::Internal(target) = target {
                    pending.push(*target);
                }
            }
            Insn::Branch { op: 0xa8, .. } | Insn::BranchW { op: 0xc9, .. } => return None,
            Insn::Branch { target, .. } | Insn::BranchW { target, .. } => {
                pending.push(next);
                if let BranchTarget::Internal(target) = target {
                    pending.push(*target);
                }
            }
            Insn::TableSwitch {
                default, targets, ..
            } => {
                pending.push(*default);
                pending.extend(targets.iter().copied());
            }
            Insn::LookupSwitch { default, pairs } => {
                pending.push(*default);
                pending.extend(pairs.iter().map(|(_, target)| *target));
            }
        }
    }
    Some(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::jvm::classfile::ClassWriter;
    use crate::jvm::classreader::{MethodCode, C};

    #[test]
    fn splice_unified_branchless_drops_return_and_stores_args() {
        // Body of `inline fun triple(x: Int): Int = x * 3` — `iload_0; iconst_3; imul; ireturn`.
        let body = MethodCode {
            max_stack: 2,
            max_locals: 1,
            code: vec![0x1a, 0x06, 0x68, 0xac],
            source_cp: vec![C::Other],
            stackmap: None,
            handlers: vec![],
            locals: vec![],
            lines: Vec::new(),
            source_file: None,
            defining_class: "T".into(),
            dependency_source_map: None,
            bootstrap_methods: Vec::new(),
        };
        let mut cw = ClassWriter::new("T", "java/lang/Object");
        let bs = super::super::splice_unified(
            &body,
            "(I)I",
            3,
            super::super::ParameterBinding::Stored(&[]),
            0,
            &mut cw,
            &super::super::ReifiedArguments::default(),
        )
        .expect("branchless splice");
        // Prologue stores the one arg into slot 3, then the body runs with no trailing return.
        // istore_3 ; iload_3 ; iconst_3 ; imul   (compact slot-3 forms; the `ireturn` is dropped)
        assert_eq!(bs.bytes, vec![0x3e, 0x1d, 0x06, 0x68]);
        // A pure branchless body needs no join frame — appendable at any operand-stack height.
        assert!(!bs.join_required);
        assert!(bs.falls_through);
    }

    #[test]
    fn splice_unified_publishes_a_physically_terminal_body() {
        // `inline fun fail(): Nothing = throw null` — no physical return reaches the call site.
        let body = MethodCode {
            max_stack: 1,
            max_locals: 0,
            code: vec![0x01, 0xbf],
            source_cp: vec![C::Other],
            stackmap: None,
            handlers: vec![],
            locals: vec![],
            lines: Vec::new(),
            source_file: None,
            defining_class: "T".into(),
            dependency_source_map: None,
            bootstrap_methods: Vec::new(),
        };
        let mut cw = ClassWriter::new("T", "java/lang/Object");
        let splice = super::super::splice_unified(
            &body,
            "()Ljava/lang/Void;",
            0,
            super::super::ParameterBinding::Stored(&[]),
            0,
            &mut cw,
            &super::super::ReifiedArguments::default(),
        )
        .expect("terminal branchless splice");

        assert_eq!(splice.bytes, vec![0x01, 0xbf]);
        assert!(!splice.join_required);
        assert!(!splice.falls_through);
    }

    #[test]
    fn splice_continuation_ignores_an_unreachable_return_after_a_loop() {
        let instructions = vec![
            Insn::Branch {
                op: 0xa7,
                target: BranchTarget::Internal(0),
            },
            Insn::Plain {
                op: 0xb0,
                operands: Vec::new(),
            },
        ];
        assert_eq!(
            caller_continuation_reachable(&instructions, &[0, 3, 4], &[]),
            Some(false)
        );
    }

    #[test]
    fn splice_continuation_follows_both_conditional_successors() {
        let instructions = vec![
            Insn::Branch {
                op: 0x99,
                target: BranchTarget::Internal(2),
            },
            Insn::Plain {
                op: 0xbf,
                operands: Vec::new(),
            },
        ];
        assert_eq!(
            caller_continuation_reachable(&instructions, &[0, 3, 4], &[]),
            Some(true)
        );
    }

    #[test]
    fn splice_continuation_rejects_legacy_subroutine_control_flow() {
        let internal = BranchTarget::Internal(0);
        let cases = [
            Insn::Branch {
                op: 0xa8,
                target: internal,
            },
            Insn::BranchW {
                op: 0xc9,
                target: internal,
            },
            Insn::Plain {
                op: 0xa9,
                operands: vec![0],
            },
            Insn::Plain {
                op: 0xc4,
                operands: vec![0xa9, 0, 0],
            },
        ];
        for instruction in cases {
            assert_eq!(
                caller_continuation_reachable(&[instruction], &[0, 1], &[]),
                None
            );
        }
    }
}
