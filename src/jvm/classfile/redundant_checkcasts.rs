//! Selection for kotlinc's redundant-`checkcast` elimination pass.
//!
//! The pass removes a cast only when the verifier state already has exactly the requested class
//! (or `null`). Exact verifier identity is sufficient: no hierarchy query or source-local spelling
//! is needed, and a broader declared LVT type must not hide a narrower fact established on every
//! incoming bytecode edge.

use super::bytecode_analysis::{FrameTypes, VerificationType};
use super::ClassWriter;
use crate::jvm::inline::Insn;

/// One bit per original instruction: the casts that can be removed without changing verifier or
/// runtime behavior. `types` is lazy because methods with a reified marker keep every cast.
pub(crate) fn select<'a>(
    writer: &ClassWriter,
    insns: &[Insn],
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
            VerificationType::Reference(name) => **name == *cast,
            _ => false,
        };
        selected[at] = flowed;
    }
    selected
}
