//! Plan the bytecode rewrite that reads an `@InlineOnly` call's arguments in place.
//!
//! The caller has already pushed arguments in source order. This module proves that the callee's
//! leading parameter loads consume that exact stack layout and records the load deletions/dups.

use std::collections::HashSet;

use super::local_compaction::LocalCompaction;
use super::{
    class_name, disassemble, incremented_local_slot, loaded_local, name_and_type,
    null_check_deletions, old_offsets, stored_local, Insn, MethodCode, C,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum LoadRewrite {
    /// The argument is already present at its first parameter load.
    Delete,
    /// The same parameter is loaded again while its argument is still the stack-top value.
    Duplicate { wide: bool },
}

impl LoadRewrite {
    pub(super) fn replacement(self) -> Vec<Insn> {
        match self {
            Self::Delete => Vec::new(),
            Self::Duplicate { wide } => vec![Insn::Plain {
                op: if wide { 0x5c } else { 0x59 },
                operands: Vec::new(),
            }],
        }
    }
}

#[derive(Clone, Debug)]
pub(in crate::jvm) struct InPlacePlan {
    rewrites: Vec<(usize, LoadRewrite)>,
    compaction: LocalCompaction,
}

impl InPlacePlan {
    pub(in crate::jvm) fn for_body(body: &MethodCode, descriptor: &str) -> Option<Self> {
        let insns = disassemble(&body.code)?;
        let stores = super::param_store_ops(descriptor, 0)?;
        if stores.is_empty() {
            return None;
        }
        let widths: Vec<u16> = stores
            .iter()
            .map(|&(_, op)| if matches!(op, 0x37 | 0x39) { 2 } else { 1 })
            .collect();
        let argument_end = stores
            .last()
            .map(|&(slot, op)| slot + if matches!(op, 0x37 | 0x39) { 2 } else { 1 })?;
        let offsets = old_offsets(&body.code)?;
        let protected_starts: HashSet<usize> = body
            .handlers
            .iter()
            .filter_map(|handler| {
                offsets
                    .iter()
                    .position(|&at| at == handler.start_pc as usize)
            })
            .collect();
        let null_checks = null_check_deletions(&insns, &body.source_cp);
        let mut rewrites = Vec::new();
        let mut expected = 0u16;
        let mut parameter = 0usize;
        let mut at = 0usize;
        while expected < argument_end {
            let insn = insns.get(at)?;
            if protected_starts.contains(&at) || prohibited(insn) && !null_checks.contains(&at) {
                return None;
            }
            if null_checks.contains(&at) {
                at += 1;
                continue;
            }
            if !whitelisted_static(insn, body)
                || stored_local(insn).is_some_and(|slot| slot < argument_end)
                || incremented_local_slot(insn).is_some_and(|slot| slot < argument_end)
            {
                return None;
            }
            match loaded_local(insn) {
                Some(slot) if slot == expected => {
                    rewrites.push((at, LoadRewrite::Delete));
                    at += 1;
                    let mut repeated = false;
                    while insns.get(at).is_some_and(|next| {
                        loaded_local(next) == Some(slot) && opcode_of(next) == opcode_of(insn)
                    }) {
                        repeated = true;
                        rewrites.push((
                            at,
                            LoadRewrite::Duplicate {
                                wide: widths[parameter] == 2,
                            },
                        ));
                        at += 1;
                    }
                    // Before any body instruction runs, the caller's last argument is the only
                    // parameter at stack top. Duplicating an earlier parameter would duplicate a
                    // later argument instead (`p0, p1; dup` duplicates `p1`). Realizing broader
                    // repeated-load plans requires moving each argument to its load site.
                    if repeated && parameter + 1 != widths.len() {
                        return None;
                    }
                    expected += widths[parameter];
                    parameter += 1;
                }
                Some(slot) if slot < argument_end => return None,
                // Anything else between loads would have to move with its argument.
                _ => return None,
            }
        }
        let touches_parameter = |insn: &Insn| {
            loaded_local(insn)
                .or_else(|| stored_local(insn))
                .or_else(|| incremented_local_slot(insn))
                .is_some_and(|slot| slot < argument_end)
        };
        if insns[at..]
            .iter()
            .enumerate()
            .any(|(offset, insn)| !null_checks.contains(&(at + offset)) && touches_parameter(insn))
        {
            return None;
        }
        Some(Self {
            rewrites,
            compaction: LocalCompaction::all_parameters(descriptor)?,
        })
    }

    pub(super) fn rewrites(&self) -> &[(usize, LoadRewrite)] {
        &self.rewrites
    }

    pub(super) fn compaction(&self) -> &LocalCompaction {
        &self.compaction
    }
}

fn prohibited(insn: &Insn) -> bool {
    let op = match insn {
        Insn::Plain { op, .. } => *op,
        Insn::Branch { op, .. } | Insn::BranchW { op, .. } => *op,
        Insn::TableSwitch { .. } | Insn::LookupSwitch { .. } => return true,
    };
    matches!(
        op,
        0x99..=0xb1
            | 0xc6
            | 0xc7
            | 0xbf
            | 0xb3
            | 0xb5
            | 0xb6..=0xba
            | 0xc2
            | 0xc3
            | 0x6c
            | 0x6d
            | 0x70
            | 0x71
            | 0xc0
            | 0xbc
            | 0xbd
            | 0xc5
            | 0x2e..=0x35
            | 0x4f..=0x56
    )
}

fn opcode_of(insn: &Insn) -> Option<u8> {
    match insn {
        Insn::Plain { op, operands } if *op == 0xc4 => operands.first().copied(),
        Insn::Plain { op, .. } => Some(match *op {
            0x1a..=0x1d => 0x15,
            0x1e..=0x21 => 0x16,
            0x22..=0x25 => 0x17,
            0x26..=0x29 => 0x18,
            0x2a..=0x2d => 0x19,
            other => other,
        }),
        _ => None,
    }
}

fn whitelisted_static(insn: &Insn, body: &MethodCode) -> bool {
    let Insn::Plain { op: 0xb2, operands } = insn else {
        return true;
    };
    let Some(index) = operands
        .first()
        .zip(operands.get(1))
        .map(|(&high, &low)| (u16::from(high) << 8) | u16::from(low))
    else {
        return false;
    };
    let Some(C::Fieldref(class, names)) = body.source_cp.get(index as usize) else {
        return false;
    };
    matches!(
        (
            class_name(&body.source_cp, *class),
            name_and_type(&body.source_cp, *names),
        ),
        (
            Some("kotlin/Result"),
            Some(("Companion", "Lkotlin/Result$Companion;"))
        ) | (Some("kotlin/_Assertions"), Some(("ENABLED", "Z")))
    )
}

#[cfg(test)]
mod tests {
    use super::super::{splice_unified, spliced_frame, ParameterBinding, ReifiedArguments};
    use super::*;
    use crate::jvm::classfile::ClassWriter;

    fn body(code: Vec<u8>, max_locals: u16) -> MethodCode {
        MethodCode {
            max_stack: 3,
            max_locals,
            code,
            source_cp: vec![C::Other],
            stackmap: None,
            handlers: vec![],
            locals: vec![],
            lines: Vec::new(),
            source_file: None,
            defining_class: "T".into(),
            dependency_source_map: None,
            bootstrap_methods: Vec::new(),
        }
    }

    #[test]
    fn leading_parameter_loads_are_planned_in_source_order() {
        let plan = InPlacePlan::for_body(&body(vec![0x1a, 0x1b, 0x60, 0xac], 2), "(II)I")
            .expect("in-place plan");
        assert_eq!(
            plan.rewrites(),
            &[(0, LoadRewrite::Delete), (1, LoadRewrite::Delete)]
        );
    }

    #[test]
    fn repeated_last_parameter_load_duplicates_the_stack_top() {
        let plan = InPlacePlan::for_body(&body(vec![0x1a, 0x1a, 0x60, 0xac], 1), "(I)I")
            .expect("in-place plan");
        assert_eq!(
            plan.rewrites(),
            &[
                (0, LoadRewrite::Delete),
                (1, LoadRewrite::Duplicate { wide: false })
            ]
        );
    }

    #[test]
    fn repeated_non_top_parameter_is_rejected_for_multiple_parameters() {
        // The incoming stack is `[first, second]`; deleting `iload_0` leaves `second` on top, so
        // replacing the second `iload_0` with `dup` would duplicate the wrong parameter.
        let candidate = body(vec![0x1a, 0x1a, 0x1b, 0x60, 0x60, 0xac], 2);
        assert!(InPlacePlan::for_body(&candidate, "(II)I").is_none());
    }

    #[test]
    fn a_parameter_read_again_after_body_code_is_rejected() {
        let candidate = body(vec![0x1a, 0x1a, 0x60, 0x1a, 0x60, 0xac], 1);
        assert!(InPlacePlan::for_body(&candidate, "(I)I").is_none());
    }

    #[test]
    fn parameters_loaded_out_of_order_are_rejected() {
        let candidate = body(vec![0x1b, 0x1a, 0x64, 0xac], 2);
        assert!(InPlacePlan::for_body(&candidate, "(II)I").is_none());
    }

    #[test]
    fn leading_parameter_loads_are_rewritten_in_place() {
        let body = body(vec![0x1a, 0x1b, 0x60, 0xac], 2);
        let plan = InPlacePlan::for_body(&body, "(II)I").expect("in-place plan");
        let mut writer = ClassWriter::new("T", "java/lang/Object");
        let splice = splice_unified(
            &body,
            "(II)I",
            3,
            ParameterBinding::InPlace(&plan),
            0,
            &mut writer,
            &ReifiedArguments::default(),
        )
        .expect("splice");
        assert_eq!(splice.bytes, vec![0x60]);
    }

    #[test]
    fn repeated_last_parameter_load_is_rewritten_to_dup() {
        let body = body(vec![0x2a, 0x2a, 0xbe, 0x57, 0xb0], 1);
        let plan = InPlacePlan::for_body(&body, "([I)[I").expect("in-place plan");
        let mut writer = ClassWriter::new("T", "java/lang/Object");
        let splice = splice_unified(
            &body,
            "([I)[I",
            3,
            ParameterBinding::InPlace(&plan),
            0,
            &mut writer,
            &ReifiedArguments::default(),
        )
        .expect("splice");
        assert_eq!(splice.bytes, vec![0x59, 0xbe, 0x57]);
    }

    #[test]
    fn wide_parameters_compact_branch_frames_body_locals_and_top_local() {
        let body = MethodCode {
            max_stack: 3,
            max_locals: 4,
            code: vec![
                0x1e, 0x1c, 0x88, 0x60, 0x3e, 0x1d, 0x99, 0x00, 0x05, 0x04, 0xac, 0x03, 0xac,
            ],
            source_cp: vec![C::Other],
            // full_frame at bytecode offset 11: locals = [long, int, int], stack = [].
            stackmap: Some(vec![
                0x00, 0x01, 0xff, 0x00, 0x0b, 0x00, 0x03, 0x04, 0x01, 0x01, 0x00, 0x00,
            ]),
            handlers: vec![],
            locals: vec![crate::jvm::classreader::MethodLocal {
                start_pc: 4,
                length: 9,
                slot: 3,
                name: "sum".into(),
                descriptor: "I".into(),
            }],
            lines: Vec::new(),
            source_file: None,
            defining_class: "T".into(),
            dependency_source_map: None,
            bootstrap_methods: Vec::new(),
        };
        let plan = InPlacePlan::for_body(&body, "(JI)I").expect("in-place plan");
        let frame = spliced_frame(&body, "(JI)I", &[], Some(&plan), 4).expect("frame plan");
        assert_eq!(frame.top_local, 5);

        let mut writer = ClassWriter::new("T", "java/lang/Object");
        let splice = splice_unified(
            &body,
            "(JI)I",
            4,
            ParameterBinding::InPlace(&plan),
            0,
            &mut writer,
            &ReifiedArguments::default(),
        )
        .expect("splice");
        assert_eq!(
            splice.bytes,
            vec![
                0x88, 0x60, 0x36, 0x04, 0x15, 0x04, 0x99, 0x00, 0x07, 0x04, 0xa7, 0x00, 0x04, 0x03
            ]
        );
        assert_eq!(splice.locals, vec![(2, 12, 4, "sum$iv".into(), "I".into())]);
    }

    #[test]
    fn stored_parameters_keep_the_exact_reverse_store_prologue() {
        let body = body(vec![0x1a, 0x1b, 0x60, 0xac], 2);
        let mut writer = ClassWriter::new("T", "java/lang/Object");
        let splice = splice_unified(
            &body,
            "(II)I",
            3,
            ParameterBinding::Stored(&[]),
            0,
            &mut writer,
            &ReifiedArguments::default(),
        )
        .expect("stored splice");
        assert_eq!(splice.bytes, vec![0x36, 0x04, 0x3e, 0x1d, 0x15, 0x04, 0x60]);
    }
}
