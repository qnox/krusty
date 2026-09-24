//! Selection for kotlinc's redundant-`checkcast` elimination pass.
//!
//! The pass removes a cast only when the verifier state already has exactly the requested class
//! (or `null`). Kotlin's optimizer also reads a named local as its declared LVT type; krusty requires
//! that declaration and the flowed verifier type to agree, because this analysis deliberately does
//! not consult a class hierarchy.

use super::bytecode_analysis::{FrameTypes, VerificationType};
use super::{ClassWriter, MethodInfo};
use crate::jvm::inline::Insn;

#[derive(Debug)]
struct NamedRange {
    start: usize,
    end: usize,
    ty: String,
}

fn aload_slot(insn: &Insn) -> Option<u16> {
    let Insn::Plain { op, operands } = insn else {
        return None;
    };
    match *op {
        0x19 => operands.first().map(|&slot| u16::from(slot)),
        0x2a..=0x2d => Some(u16::from(*op - 0x2a)),
        0xc4 if operands.first() == Some(&0x19) => {
            Some(u16::from_be_bytes([*operands.get(1)?, *operands.get(2)?]))
        }
        _ => None,
    }
}

/// One bit per original instruction: the casts that can be removed without changing verifier or
/// runtime behavior. `types` is lazy because methods with a reified marker keep every cast.
pub(crate) fn select<'a>(
    writer: &ClassWriter,
    method: &MethodInfo,
    insns: &[Insn],
    offsets: &[usize],
    types: impl FnOnce() -> Option<&'a FrameTypes>,
) -> Vec<bool> {
    let mut selected = vec![false; insns.len()];
    let reified = insns.iter().any(|insn| match insn {
        Insn::Plain { op: 0xb8, operands } if operands.len() == 2 => writer
            .methodref_parts(u16::from_be_bytes([operands[0], operands[1]]))
            .is_some_and(|(owner, name, _)| {
                owner == "kotlin/jvm/internal/Intrinsics" && name == "reifiedOperationMarker"
            }),
        _ => false,
    });
    if reified
        || !insns
            .iter()
            .any(|insn| matches!(insn, Insn::Plain { op: 0xc0, .. }))
    {
        return selected;
    }
    let Some(types) = types() else {
        return selected;
    };

    let code_len = offsets.last().copied().unwrap_or(0);
    let index_of = |pc: usize| offsets.binary_search(&pc).ok();
    let range = |start: Option<u16>, len: Option<u16>| {
        let start = start.map_or(0, usize::from);
        let end = len.map_or(code_len, |len| start + usize::from(len));
        (start, end.min(code_len))
    };
    let mut named: Vec<Vec<NamedRange>> = (0..usize::from(method.max_locals))
        .map(|_| Vec::new())
        .collect();
    for &(_, descriptor, slot, start, len) in &method.lvt {
        let (start, end) = range(start, len);
        if start >= code_len {
            continue;
        }
        let (Some(start), Some(end), Some(descriptor)) = (
            index_of(start),
            index_of(end),
            writer.cp.utf8_at(descriptor),
        ) else {
            continue;
        };
        let Some(ranges) = named.get_mut(usize::from(slot)) else {
            continue;
        };
        let ty = descriptor
            .strip_prefix('L')
            .and_then(|name| name.strip_suffix(';'))
            .unwrap_or(descriptor)
            .to_string();
        ranges.push(NamedRange { start, end, ty });
    }
    // Generated LVT ranges for one slot do not overlap. If malformed input does, declining casts
    // loaded from that slot is safer than choosing whichever entry happens to come first.
    let mut ambiguous = vec![false; named.len()];
    for (slot, ranges) in named.iter_mut().enumerate() {
        ranges.sort_unstable_by_key(|range| (range.start, range.end));
        ambiguous[slot] = ranges.windows(2).any(|pair| pair[0].end > pair[1].start);
    }

    for (at, insn) in insns.iter().enumerate() {
        let Insn::Plain { op: 0xc0, operands } = insn else {
            continue;
        };
        let Some(cast) = operands
            .get(..2)
            .and_then(|bytes| writer.class_name_at(u16::from_be_bytes([bytes[0], bytes[1]])))
        else {
            continue;
        };
        if cast.starts_with("[[") {
            continue;
        }
        let Some(top) = types.before(at).and_then(|state| state.stack.last()) else {
            continue;
        };
        let flowed = match top {
            VerificationType::Null => true,
            VerificationType::Reference(name) => name == cast,
            _ => false,
        };
        if !flowed {
            continue;
        }
        let declared_agrees = at
            .checked_sub(1)
            .and_then(|load| aload_slot(&insns[load]).map(|slot| (load, slot)))
            .is_none_or(|(load, slot)| {
                let slot = usize::from(slot);
                if ambiguous.get(slot).copied().unwrap_or(true) {
                    return false;
                }
                let Some(ranges) = named.get(slot) else {
                    return false;
                };
                let next = ranges.partition_point(|range| range.start <= load);
                next == 0 || ranges[next - 1].end <= load || ranges[next - 1].ty == cast
            });
        selected[at] = declared_agrees;
    }
    selected
}
