//! Stack ownership between a substituted lambda's receiver load and `FunctionN.invoke`.

use std::collections::HashSet;

use crate::jvm::classreader::C;

use super::{methodref_desc_effect, Insn};

/// Whether the value pushed at `load` stays untouched below every value computed before `invoke`.
/// Counts verifier stack entries (a `long`/`double` is one), matching method descriptors and frames.
/// Unsupported stack manipulation declines conservatively; arbitrary alias tracking belongs to the
/// symbolic inliner.
pub(super) fn survives_to_invoke(
    insns: &[Insn],
    src_cp: &[C],
    load: usize,
    invoke: usize,
    deleted: &HashSet<usize>,
) -> bool {
    let mut above = 0usize;
    for (index, instruction) in insns.iter().enumerate().take(invoke).skip(load + 1) {
        if deleted.contains(&index) {
            continue;
        }
        let Some((pops, pushes)) = stack_entry_effect(instruction, src_cp) else {
            return false;
        };
        if pops > above {
            return false;
        }
        above = above - pops + pushes;
    }
    let Some(Insn::Plain { op: 0xb9, operands }) = insns.get(invoke) else {
        return false;
    };
    let Some(pool) = operands
        .first()
        .zip(operands.get(1))
        .map(|(&high, &low)| (u16::from(high) << 8) | u16::from(low))
    else {
        return false;
    };
    methodref_desc_effect(src_cp, pool).is_some_and(|(arguments, _)| arguments == above)
}

/// `(popped entries, pushed entries)` for one straight-line JVM instruction. This deliberately
/// rejects terminals, subroutines and invokedynamic; none can be part of the direct receiver-to-call
/// shape owned by the byte splicer.
fn stack_entry_effect(instruction: &Insn, src_cp: &[C]) -> Option<(usize, usize)> {
    let (op, operands) = match instruction {
        Insn::Plain { op, operands } => (*op, operands.as_slice()),
        Insn::Branch { op, .. } | Insn::BranchW { op, .. } => (*op, [].as_slice()),
        Insn::TableSwitch { .. } | Insn::LookupSwitch { .. } => return Some((1, 0)),
    };
    Some(match op {
        0x00 | 0x84 | 0xa7 | 0xc8 => (0, 0),
        0x01..=0x2d | 0xb2 | 0xbb => (0, 1),
        0x2e..=0x35 => (2, 1),
        0x36..=0x4e | 0x57 | 0xb3 | 0xc2 | 0xc3 => (1, 0),
        0x4f..=0x56 => (3, 0),
        0x58 => (2, 0),
        0x59 => (1, 2),
        0x5a => (2, 3),
        0x5b => (3, 4),
        0x5c => (2, 4),
        0x5d => (3, 5),
        0x5e => (4, 6),
        0x5f => (2, 2),
        0x60..=0x73 | 0x78..=0x83 => (2, 1),
        0x74..=0x77 | 0x85..=0x93 | 0xc0 | 0xc1 => (1, 1),
        0x94..=0x98 => (2, 1),
        0x99..=0x9e | 0xc6 | 0xc7 => (1, 0),
        0x9f..=0xa6 => (2, 0),
        0xb4 => (1, 1),
        0xb5 => (2, 0),
        0xb6..=0xb9 => {
            let pool = operands
                .first()
                .zip(operands.get(1))
                .map(|(&high, &low)| (u16::from(high) << 8) | u16::from(low))?;
            let (arguments, result) = methodref_desc_effect(src_cp, pool)?;
            (
                arguments + usize::from(op != 0xb8),
                usize::from(result.is_some()),
            )
        }
        0xbc | 0xbd | 0xbe => (1, 1),
        0xc4 => match *operands.first()? {
            0x15..=0x19 => (0, 1),
            0x36..=0x3a => (1, 0),
            0x84 => (0, 0),
            _ => return None,
        },
        0xc5 => (usize::from(*operands.get(2)?), 1),
        _ => return None,
    })
}
