//! Reified-operation marker recognition and concrete type-operand rewriting.

use super::{methodref_target, set_pool_operand, utf8, Insn};
use crate::jvm::classfile::ClassWriter;
use crate::jvm::classreader::C;

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
/// pool `idx`. Handles the 2-byte type-ops / `ldc_w` (via [`set_pool_operand`]) and the 1-byte `ldc`
/// (widening it to `ldc_w` when `idx` exceeds one byte). Returns `false` only for a malformed
/// compact `ldc` operand, so the caller can skip the splice instead of miscompiling.
pub(super) fn set_reified_operand(insn: &mut Insn, idx: u16) -> bool {
    if let Insn::Plain { op, operands } = insn {
        if *op == 0x12 {
            if operands.is_empty() {
                return false;
            }
            if idx > 0xff {
                // `ldc` carries a ONE-byte pool index, and the concrete type's index in the HOST
                // class can be anything — a file with a few hundred constants pushes it past a
                // byte. Widen to `ldc_w` (0x13), the identical-semantics 2-byte form, exactly as
                // `relocate_insns` does for the same overflow. Branch targets and frames are keyed
                // by instruction INDEX, not byte offset, so the size change is handled downstream.
                *op = 0x13;
                *operands = vec![(idx >> 8) as u8, (idx & 0xff) as u8];
                return true;
            }
            operands[0] = idx as u8;
            return true;
        }
    }
    set_pool_operand(insn, idx);
    true
}

/// Substitute Kotlin's `reifiedOperationMarker` pattern in an inline body: the call (preceded by its
/// `iconst <mode>` and `ldc "<typeParam>"` argument pushes) is replaced with `nop`s, and the
/// following type op (`anewarray`/`checkcast`/`instanceof`) is repointed at the concrete reified type
/// from `type_map` (Kotlin type-parameter name → JVM internal name). Returns the `(insn index, target
/// pool index)` repoints to apply *after* relocation (so the type op isn't re-relocated to `Object`).
/// This is how `emptyArray<String>()` inlines to `anewarray java/lang/String`.
pub fn substitute_reified(
    insns: &mut [Insn],
    src_cp: &[C],
    cw: &mut ClassWriter,
    type_map: &std::collections::HashMap<String, String>,
) -> Vec<(usize, u16)> {
    let mut patches = Vec::new();
    for i in 0..insns.len() {
        // The marker is `invokestatic kotlin/jvm/internal/Intrinsics.reifiedOperationMarker`.
        let is_marker = matches!(&insns[i], Insn::Plain { op: 0xb8, operands } if operands.len() == 2
            && methodref_target(src_cp, (operands[0] as u16) << 8 | operands[1] as u16)
                == Some(("kotlin/jvm/internal/Intrinsics", "reifiedOperationMarker")));
        if !is_marker || i < 2 {
            continue;
        }
        // The `ldc "<typeParam>"` immediately before names the reified parameter.
        let name = match &insns[i - 1] {
            Insn::Plain { op: 0x12, operands } if operands.len() == 1 => match src_cp
                .get(operands[0] as usize)
            {
                Some(C::String(u)) => utf8(src_cp, *u).map(|s| s.trim_end_matches('?').to_string()),
                _ => None,
            },
            _ => None,
        };
        // Erase the marker call and its two argument pushes (mode + type-name).
        let nop = Insn::Plain {
            op: 0x00,
            operands: vec![],
        };
        insns[i] = nop.clone();
        insns[i - 1] = nop.clone();
        insns[i - 2] = nop;
        // Repoint the next type op at the concrete type.
        if let Some(name) = name {
            if let Some(concrete) = type_map.get(&name) {
                if let Some(j) = (i + 1..insns.len()).find(|&j| is_type_op(&insns[j])) {
                    patches.push((j, cw.class_ref(concrete)));
                }
            }
        }
    }
    patches
}

/// NOP each `reifiedOperationMarker` triplet (`iconst <mode>; ldc "<T>"; invokestatic marker`) in place
/// — the marker itself is a compile-time directive that THROWS at runtime, so it must never reach the
/// spliced bytecode. Returns, per marker, the index of the following type-bearing instruction
/// (`anewarray`/`checkcast`/…/`ldc class`) and the reified type-parameter name — so the caller repoints
/// that instruction at the concrete type AFTER relocation (`set_reified_operand`). Unlike
/// [`substitute_reified`], this also covers the `ldc class` (KClass / `T::class`) modes and defers the
/// operand rewrite (the CLASS pool ref must be minted post-relocation to survive `relocate_insns`).
/// `None` (⇒ the caller SKIPS the whole splice, never miscompiles) if any marker is malformed — the
/// preceding `ldc "<T>"` name is unreadable, or no type-bearing op follows — since NOP-ing an
/// unspecializable marker would leave the erased placeholder in the emitted body.
pub(super) fn reify_markers(insns: &mut [Insn], src_cp: &[C]) -> Option<Vec<(usize, String)>> {
    // Locate every marker + its (name, following type-bearing index) FIRST, bailing on any malformed
    // one, so a partial NOP is never left behind when we decide to skip.
    let mut plan: Vec<(usize, usize, String)> = Vec::new();
    for i in 0..insns.len() {
        let is_marker = matches!(&insns[i], Insn::Plain { op: 0xb8, operands } if operands.len() == 2
            && methodref_target(src_cp, (operands[0] as u16) << 8 | operands[1] as u16)
                == Some(("kotlin/jvm/internal/Intrinsics", "reifiedOperationMarker")));
        if !is_marker {
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
        let name = name_idx.and_then(|idx| match src_cp.get(idx) {
            Some(C::String(u)) => utf8(src_cp, *u).map(|s| s.trim_end_matches('?').to_string()),
            _ => None,
        })?;
        let j = (i + 1..insns.len()).find(|&j| is_reified_type_bearing(&insns[j], src_cp))?;
        plan.push((i, j, name));
    }
    let nop = Insn::Plain {
        op: 0x00,
        operands: vec![],
    };
    let mut targets = Vec::with_capacity(plan.len());
    for (i, j, name) in plan {
        insns[i] = nop.clone();
        insns[i - 1] = nop.clone();
        insns[i - 2] = nop.clone();
        targets.push((j, name));
    }
    Some(targets)
}

#[cfg(test)]
mod tests {
    use super::set_reified_operand;
    use crate::jvm::inline::Insn;

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
    fn malformed_compact_ldc_declines_the_reified_repoint() {
        let mut instruction = Insn::Plain {
            op: 0x12,
            operands: Vec::new(),
        };

        assert!(!set_reified_operand(&mut instruction, 1));
    }
}
