//! The raw-byte splicer's adapter for reified operations: marker recognition and concrete
//! type-operand rewriting, over the neutral [`ReifiedArguments`] contract. Anything it cannot rewrite
//! in place (a nullable `instanceof`) declines the whole splice to the symbolic inliner.

use super::relocation::narrowest_ldc;
use super::{invoked_method, set_pool_operand, utf8, Insn};
use crate::jvm::classfile::ClassWriter;
use crate::jvm::classreader::C;
use crate::jvm::reified_arguments::{ReifiedArgument, ReifiedArguments};
use crate::jvm::type_of::TYPE_OF_MARKER;

/// True for the type-bearing ops a `reifiedOperationMarker` precedes: `anewarray`, `checkcast`,
/// `instanceof`, `multianewarray`.
fn is_type_op(insn: &Insn) -> bool {
    matches!(
        insn,
        Insn::Plain {
            op: 0xbd | 0xc0 | 0xc1 | 0xc5,
            ..
        }
    )
}

/// True for an `ldc`/`ldc_w` that pushes a `Class` constant — the type-bearing op for the KClass /
/// `T::class(.java)` reified modes (`reifiedOperationMarker(4|5, "T")` then `ldc class <erased>`,
/// followed for `KClass` by `Reflection.getOrCreateKotlinClass`). The erased `ldc class` operand is what
/// specializes to the concrete reified type.
fn is_ldc_class(insn: &Insn, src_cp: &[C]) -> bool {
    let (op, operands) = match insn {
        Insn::Plain { op, operands } => (*op, operands),
        _ => return false,
    };
    let idx = match op {
        0x12 => operands.first().map(|&b| b as u16), // ldc (1-byte)
        0x13 => operands
            .first()
            .zip(operands.get(1))
            .map(|(a, b)| (*a as u16) << 8 | *b as u16), // ldc_w (2-byte)
        _ => return false,
    };
    idx.and_then(|i| src_cp.get(i as usize))
        .is_some_and(|c| matches!(c, C::Class(_)))
}

/// The type-bearing op (`anewarray`/`checkcast`/… OR an `ldc class`) a `reifiedOperationMarker` precedes.
fn is_reified_type_bearing(insn: &Insn, src_cp: &[C]) -> bool {
    is_type_op(insn) || is_ldc_class(insn, src_cp)
}

/// Overwrite the constant-pool operand of a reified type-bearing instruction with the concrete `Class`
/// pool `idx`: the 2-byte type-ops via [`set_pool_operand`], an `ldc`/`ldc_w` in the form the host
/// index needs ([`narrowest_ldc`]). Returns `false` only for a malformed compact `ldc` operand, so
/// the caller can skip the splice instead of miscompiling.
pub(super) fn set_reified_operand(insn: &mut Insn, idx: u16) -> bool {
    match insn {
        Insn::Plain { op: 0x12, operands } if operands.is_empty() => return false,
        // The concrete type's index in the HOST class can be anything, so the form the
        // placeholder took says nothing about it. Branch targets and frames are keyed by
        // instruction INDEX, not byte offset, so a size change is handled downstream.
        Insn::Plain {
            op: 0x12 | 0x13, ..
        } => *insn = narrowest_ldc(idx),
        _ => set_pool_operand(insn, idx),
    }
    true
}

/// One post-relocation rewrite of a reified marker site, keyed by instruction index.
pub(super) enum ReifiedRepoint {
    /// Point the type-bearing op (`anewarray`/`checkcast`/…/`ldc class`) at this concrete class.
    Class(usize, String),
    /// Point the marker's type-parameter `ldc` at this forwarded name.
    Marker(usize, String),
    /// Replace a `typeOf` placeholder with this marker argument's realization.
    TypeOf(usize, String),
}

impl ReifiedRepoint {
    /// Apply the rewrite with a pool entry minted in the host class. `false` for a malformed
    /// compact `ldc`, so the caller skips the splice instead of miscompiling.
    pub(super) fn apply(&self, insns: &mut [Insn], cw: &mut ClassWriter) -> bool {
        match self {
            Self::Class(at, class) => {
                let idx = cw.class_ref(class);
                set_reified_operand(&mut insns[*at], idx)
            }
            // The source may load its name with `ldc_w` (a large dependency pool); the host picks
            // the compact form whenever its own index fits, as kotlinc emits it.
            Self::Marker(at, name) => {
                insns[*at] = narrowest_ldc(cw.const_string(name));
                true
            }
            Self::TypeOf(..) => false,
        }
    }
}

/// Apply post-relocation marker rewrites and return instruction expansions plus required stack.
pub(super) fn apply_repoints(
    repoints: &[ReifiedRepoint],
    arguments: &ReifiedArguments,
    insns: &mut [Insn],
    writer: &mut ClassWriter,
) -> Option<(Vec<(usize, Vec<Insn>)>, u16)> {
    let mut edits = Vec::new();
    let mut stack_growth = 0;
    for repoint in repoints {
        if let ReifiedRepoint::TypeOf(at, argument) = repoint {
            let realization = arguments.type_of.get(argument)?;
            stack_growth = stack_growth.max(crate::jvm::type_of::max_stack(realization));
            edits.push((*at, crate::jvm::type_of::encode_insns(realization, writer)));
        } else if !repoint.apply(insns, writer) {
            return None;
        }
    }
    Some((edits, stack_growth))
}

/// `invokestatic kotlin/jvm/internal/Intrinsics.reifiedOperationMarker(ILjava/lang/String;)V`,
/// exactly: opcode, owner, name, descriptor and a class (not interface) method reference. Another
/// overload or owner is an ordinary call.
fn is_marker_call(insn: &Insn, src_cp: &[C]) -> bool {
    let Insn::Plain { op: 0xb8, operands } = insn else {
        return false;
    };
    let class_method = matches!(
        operands.as_slice(),
        [high, low] if matches!(
            src_cp.get(usize::from(u16::from(*high) << 8 | u16::from(*low))),
            Some(C::Methodref(..))
        )
    );
    class_method
        && invoked_method(insn, src_cp)
            == Some((
                "kotlin/jvm/internal/Intrinsics",
                "reifiedOperationMarker",
                "(ILjava/lang/String;)V",
                false,
            ))
}

/// The marker's operation kind, pushed by the instruction two before the call.
fn marker_operation(insn: &Insn) -> Option<i32> {
    match insn {
        Insn::Plain { op, operands } if (0x02..=0x08).contains(op) && operands.is_empty() => {
            Some(i32::from(*op) - 0x03)
        }
        Insn::Plain { op: 0x10, operands } if operands.len() == 1 => {
            Some(i32::from(operands[0] as i8))
        }
        _ => None,
    }
}

/// Specialize each `reifiedOperationMarker` triplet (`iconst <mode>; ldc "<T>"; invokestatic marker`)
/// for the call's `reified` arguments. A concrete argument NOPs the triplet in place — the marker is
/// a compile-time directive that THROWS at runtime, so it must never reach the spliced bytecode —
/// and repoints the following type-bearing instruction (`anewarray`/`checkcast`/…/`ldc class`). A
/// forwarded argument keeps the triplet and renames its type parameter. A `typeOf` marker (mode 6)
/// NOPs the triplet and leaves its `aconst_null` placeholder for the caller to replace. Rewrites are returned for the
/// caller to apply AFTER relocation, so the fresh pool refs survive `relocate_insns`. `None` (⇒ the
/// caller SKIPS the whole splice, never miscompiles) if any marker is malformed — the preceding
/// `ldc "<T>"` name is unreadable, or no type-bearing op follows — or names a parameter `reified`
/// lacks, since an unspecialized marker would leave the erased placeholder in the emitted body.
pub(super) fn reify_markers(
    insns: &mut [Insn],
    src_cp: &[C],
    reified: &ReifiedArguments,
) -> Option<Vec<ReifiedRepoint>> {
    // Plan every marker FIRST, bailing on any malformed one, so a partial NOP is never left behind
    // when we decide to skip.
    let mut plan: Vec<(usize, ReifiedRepoint)> = Vec::new();
    for i in 0..insns.len() {
        if !is_marker_call(&insns[i], src_cp) {
            continue;
        }
        if i < 2 {
            return None; // a marker with no room for its `iconst <mode>; ldc "<T>"` argument pushes
        }
        // The type-parameter name is pushed by the `ldc`/`ldc_w` before the marker. A large class (e.g.
        // stdlib `CollectionsKt`) has a constant pool past 255, so the name String is loaded with `ldc_w`
        // (0x13, 2-byte index), not `ldc` (0x12, 1-byte) — read BOTH, else the marker looks malformed and
        // an otherwise-splicable reified inline (`filterIsInstance`) is wrongly skipped.
        let name_idx = match &insns[i - 1] {
            Insn::Plain { op: 0x12, operands } if operands.len() == 1 => Some(operands[0] as usize),
            Insn::Plain { op: 0x13, operands } if operands.len() == 2 => {
                Some(((operands[0] as usize) << 8) | operands[1] as usize)
            }
            _ => None,
        };
        let marker = name_idx.and_then(|idx| match src_cp.get(idx) {
            Some(C::String(u)) => utf8(src_cp, *u),
            _ => None,
        })?;
        if marker_operation(&insns[i - 2]) == Some(TYPE_OF_MARKER) {
            if !matches!(insns.get(i + 1), Some(Insn::Plain { op: 0x01, .. })) {
                return None;
            }
            plan.push((i, ReifiedRepoint::TypeOf(i + 1, marker.to_owned())));
            continue;
        }
        let j = (i + 1..insns.len()).find(|&j| is_reified_type_bearing(&insns[j], src_cp))?;
        let repoint = match reified.classes.get(marker.trim_end_matches('?'))? {
            // A nullable `instanceof` becomes kotlinc's null-accepting sequence, which needs new
            // branches; only the symbolic inliner inserts those.
            ReifiedArgument::Class { nullable, .. }
                if (*nullable || marker.ends_with('?'))
                    && matches!(insns[j], Insn::Plain { op: 0xc1, .. }) =>
            {
                crate::trace_compiler!("splice", "nullable reified instanceof is not spliceable");
                return None;
            }
            ReifiedArgument::Class { internal, .. } => ReifiedRepoint::Class(j, internal.clone()),
            ReifiedArgument::Forwarded { name, nullable } => {
                let nullable = *nullable || marker.ends_with('?');
                ReifiedRepoint::Marker(i - 1, format!("{name}{}", if nullable { "?" } else { "" }))
            }
        };
        plan.push((i, repoint));
    }
    let nop = Insn::Plain {
        op: 0x00,
        operands: vec![],
    };
    let mut repoints = Vec::with_capacity(plan.len());
    for (i, repoint) in plan {
        if matches!(
            repoint,
            ReifiedRepoint::Class(..) | ReifiedRepoint::TypeOf(..)
        ) {
            insns[i] = nop.clone();
            insns[i - 1] = nop.clone();
            insns[i - 2] = nop.clone();
        }
        repoints.push(repoint);
    }
    Some(repoints)
}

#[cfg(test)]
mod tests {
    use super::{reify_markers, set_reified_operand, ReifiedRepoint};
    use crate::jvm::classfile::ClassWriter;
    use crate::jvm::classreader::C;
    use crate::jvm::inline::Insn;
    use crate::jvm::reified_arguments::{ReifiedArgument, ReifiedArguments};
    use std::collections::HashMap;

    fn array_marker() -> (Vec<C>, Vec<Insn>) {
        let pool = vec![
            C::Other,
            C::Utf8("kotlin/jvm/internal/Intrinsics".into()),
            C::Class(1),
            C::Utf8("reifiedOperationMarker".into()),
            C::Utf8("(ILjava/lang/String;)V".into()),
            C::NameAndType(3, 4),
            C::Methodref(2, 5),
            C::Utf8("T?".into()),
            C::String(7),
            C::Utf8("java/lang/Object".into()),
            C::Class(9),
        ];
        let instructions = vec![
            Insn::Plain {
                op: 0x03,
                operands: vec![],
            },
            Insn::Plain {
                op: 0x12,
                operands: vec![8],
            },
            Insn::Plain {
                op: 0xb8,
                operands: vec![0, 6],
            },
            Insn::Plain {
                op: 0xbd,
                operands: vec![0, 10],
            },
        ];
        (pool, instructions)
    }

    #[test]
    fn a_concrete_argument_removes_the_marker_and_repoints_the_type_operation() {
        let (pool, mut instructions) = array_marker();
        let arguments = ReifiedArguments {
            classes: HashMap::from([(
                "T".to_owned(),
                ReifiedArgument::Class {
                    internal: "java/lang/String".to_owned(),
                    nullable: false,
                    intrinsic: None,
                    rendered: String::new(),
                },
            )]),
            ..Default::default()
        };

        let repoints = reify_markers(&mut instructions, &pool, &arguments).expect("valid marker");
        assert_eq!(repoints.len(), 1);
        assert!(
            matches!(repoints[0], ReifiedRepoint::Class(3, ref name) if name == "java/lang/String")
        );
        assert!(instructions[..3]
            .iter()
            .all(|instruction| matches!(instruction, Insn::Plain { op: 0x00, operands } if operands.is_empty())));

        let mut writer = ClassWriter::new("Caller", "java/lang/Object");
        assert!(repoints[0].apply(&mut instructions, &mut writer));
        let expected = writer.class_ref("java/lang/String");
        assert!(matches!(
            &instructions[3],
            Insn::Plain { op: 0xbd, operands }
                if operands == &expected.to_be_bytes()
        ));
    }

    #[test]
    fn a_forwarded_argument_keeps_the_marker_and_renames_its_nullable_parameter() {
        let (pool, mut instructions) = array_marker();
        let original = instructions.clone();
        let arguments = ReifiedArguments {
            classes: HashMap::from([(
                "T".to_owned(),
                ReifiedArgument::Forwarded {
                    name: "U".to_owned(),
                    nullable: false,
                },
            )]),
            ..Default::default()
        };

        let repoints = reify_markers(&mut instructions, &pool, &arguments).expect("valid marker");
        assert_eq!(
            instructions, original,
            "forwarding preserves the marker triplet"
        );
        assert!(matches!(repoints.as_slice(), [ReifiedRepoint::Marker(1, name)] if name == "U?"));

        let mut writer = ClassWriter::new("Caller", "java/lang/Object");
        assert!(repoints[0].apply(&mut instructions, &mut writer));
        let expected = writer.const_string("U?");
        assert!(matches!(
            &instructions[1],
            Insn::Plain { op: 0x12, operands } if operands == &[expected as u8]
        ));
        assert_eq!(instructions[3], original[3], "the erased placeholder stays");
    }

    #[test]
    fn an_unbound_marker_declines_without_mutating_the_body() {
        let (pool, mut instructions) = array_marker();
        let original = instructions.clone();

        assert!(reify_markers(&mut instructions, &pool, &ReifiedArguments::default()).is_none());
        assert_eq!(instructions, original);
    }

    /// A nullable `instanceof` needs kotlinc's null-accepting branches, which only the symbolic
    /// inliner inserts. The adapter declines the WHOLE body: the array marker before it, which it
    /// could rewrite, is left exactly as it was.
    #[test]
    fn a_nullable_instance_check_declines_the_whole_body_unchanged() {
        let (pool, mut instructions) = array_marker();
        let marker = instructions[..3].to_vec();
        instructions.extend(marker);
        instructions.push(Insn::Plain {
            op: 0xc1,
            operands: vec![0, 10],
        });
        let original = instructions.clone();
        let arguments = ReifiedArguments {
            classes: HashMap::from([(
                "T".to_owned(),
                ReifiedArgument::Class {
                    internal: "java/lang/String".to_owned(),
                    nullable: true,
                    intrinsic: None,
                    rendered: "kotlin.String?".to_owned(),
                },
            )]),
            ..Default::default()
        };

        assert!(reify_markers(&mut instructions, &pool, &arguments).is_none());
        assert_eq!(instructions, original);
    }

    /// Only the exact `reifiedOperationMarker(ILjava/lang/String;)V` is the compiler's marker. The
    /// same name with another descriptor is an ordinary call: nothing to specialize, nothing
    /// declined — where the real marker, unbound, would decline.
    #[test]
    fn another_overload_of_the_marker_name_is_an_ordinary_call() {
        let (mut pool, mut instructions) = array_marker();
        pool[4] = C::Utf8("(ILjava/lang/Object;)V".into());
        let original = instructions.clone();

        let repoints = reify_markers(&mut instructions, &pool, &ReifiedArguments::default())
            .expect("an ordinary call needs no reified argument");
        assert!(repoints.is_empty());
        assert_eq!(instructions, original);
    }

    #[test]
    fn compact_ldc_stays_compact_when_the_host_index_fits() {
        let mut instruction = Insn::Plain {
            op: 0x12,
            operands: vec![7],
        };

        assert!(set_reified_operand(&mut instruction, 0xfe));
        assert!(matches!(
            instruction,
            Insn::Plain {
                op: 0x12,
                ref operands,
            } if operands == &[0xfe]
        ));
    }

    #[test]
    fn compact_ldc_widens_when_the_host_index_exceeds_a_byte() {
        let mut instruction = Insn::Plain {
            op: 0x12,
            operands: vec![7],
        };

        assert!(set_reified_operand(&mut instruction, 0x123));
        assert!(matches!(
            instruction,
            Insn::Plain {
                op: 0x13,
                ref operands,
            } if operands == &[0x01, 0x23]
        ));
    }

    #[test]
    fn wide_ldc_narrows_when_the_host_index_fits() {
        let mut instruction = Insn::Plain {
            op: 0x13,
            operands: vec![0x01, 0x23],
        };

        assert!(set_reified_operand(&mut instruction, 0x2a));
        assert_eq!(
            instruction,
            Insn::Plain {
                op: 0x12,
                operands: vec![0x2a],
            }
        );
    }

    #[test]
    fn malformed_compact_ldc_declines_the_reified_repoint() {
        let mut instruction = Insn::Plain {
            op: 0x12,
            operands: Vec::new(),
        };

        assert!(!set_reified_operand(&mut instruction, 1));
    }
}
