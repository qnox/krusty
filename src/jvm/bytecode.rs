//! Structural operations over encoded JVM instructions.
//!
//! This boundary is deliberately independent of the emitter and inliner. Readers, relocators,
//! coroutine-marker scans, and classfile debug-table code all need to walk the same byte stream;
//! none of those consumers owns the JVM instruction format.

/// The length in bytes of the instruction at `pc` (opcode + operands), including the
/// variable-length `tableswitch`/`lookupswitch`/`wide` forms. Returns `None` for an out-of-range,
/// malformed, or truncated instruction.
pub(crate) fn instruction_len(code: &[u8], pc: usize) -> Option<usize> {
    let op = *code.get(pc)?;
    let len = match op {
        0xc4 => match code.get(pc + 1)? {
            0x84 => 6,
            0x15..=0x19 | 0x36..=0x3a | 0xa9 => 4,
            _ => return None,
        },
        0xaa => {
            let base = pc.checked_add(1)?;
            let pad = (4 - (base % 4)) % 4;
            let operands = base.checked_add(pad)?;
            let low = i32::from_be_bytes(code.get(operands + 4..operands + 8)?.try_into().ok()?);
            let high = i32::from_be_bytes(code.get(operands + 8..operands + 12)?.try_into().ok()?);
            let count = i64::from(high) - i64::from(low) + 1;
            let count = usize::try_from(count).ok().filter(|count| *count > 0)?;
            operands
                .checked_add(12)?
                .checked_add(count.checked_mul(4)?)?
                .checked_sub(pc)?
        }
        0xab => {
            let base = pc.checked_add(1)?;
            let pad = (4 - (base % 4)) % 4;
            let operands = base.checked_add(pad)?;
            let pairs = i32::from_be_bytes(code.get(operands + 4..operands + 8)?.try_into().ok()?);
            let pairs = usize::try_from(pairs).ok()?;
            operands
                .checked_add(8)?
                .checked_add(pairs.checked_mul(8)?)?
                .checked_sub(pc)?
        }
        0x10 | 0x12 | 0x15..=0x19 | 0x36..=0x3a | 0xa9 | 0xbc => 2,
        0x11
        | 0x13
        | 0x14
        | 0x84
        | 0x99..=0xa8
        | 0xb2..=0xb8
        | 0xbb
        | 0xbd
        | 0xc0
        | 0xc1
        | 0xc6
        | 0xc7 => 3,
        0xc5 => 4,
        // krusty's temporary coroutine-site marker (`impdep1` + kind + ordinal).
        0xfe => 4,
        // krusty's stand-in for one of kotlinc's `InlineMarker` calls (`impdep2` + kind).
        CODEGEN_MARKER_OP => 2,
        0xb9 | 0xba | 0xc8 | 0xc9 => 5,
        _ => 1,
    };
    pc.checked_add(len).filter(|end| *end <= code.len())?;
    Some(len)
}

/// The opcode krusty writes in place of a call of kotlinc's `kotlin/jvm/internal/InlineMarker`.
///
/// kotlinc's codegen leaves those calls in a body for the passes that follow it: the coroutine
/// transformer and FixStack read them and delete every one. Writing the calls themselves would
/// intern `InlineMarker` into the class's constant pool, where kotlinc's final class has no such
/// entry. `impdep2` (`0xff`) is reserved by JVMS §6.2 for an implementation's own use, so it is
/// written instead, and a body read into a method node gets the call back (see [`CodegenMarker`]).
pub(crate) const CODEGEN_MARKER_OP: u8 = 0xff;

/// Which `InlineMarker` method a [`CODEGEN_MARKER_OP`] stands for; its one operand byte.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CodegenMarker {
    /// `mark(I)V`: the id is pushed by an ordinary constant instruction before it.
    Mark = 1,
    BeforeInlineCall = 2,
    AfterInlineCall = 3,
}

impl CodegenMarker {
    pub(crate) const OWNER: &'static str = "kotlin/jvm/internal/InlineMarker";

    pub(crate) fn from_operand(operand: u8) -> Option<CodegenMarker> {
        Some(match operand {
            1 => CodegenMarker::Mark,
            2 => CodegenMarker::BeforeInlineCall,
            3 => CodegenMarker::AfterInlineCall,
            _ => return None,
        })
    }

    /// The `InlineMarker` method's name and descriptor.
    pub(crate) fn method(self) -> (&'static str, &'static str) {
        match self {
            CodegenMarker::Mark => ("mark", "(I)V"),
            CodegenMarker::BeforeInlineCall => ("beforeInlineCall", "()V"),
            CodegenMarker::AfterInlineCall => ("afterInlineCall", "()V"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::instruction_len;

    #[test]
    fn instruction_lengths_cover_switches_wide_forms_and_malformed_operands() {
        assert_eq!(instruction_len(&[0x10, 0x05], 0), Some(2));
        assert_eq!(instruction_len(&[0xb8, 0, 6, 0xb1], 0), Some(3));
        assert_eq!(instruction_len(&[0xc4, 0x84, 0, 1, 0, 1], 0), Some(6));
        assert_eq!(instruction_len(&[0xc8, 0, 0, 0, 4], 0), Some(5));
        assert_eq!(instruction_len(&[0x60], 0), Some(1));
        assert_eq!(instruction_len(&[0xc4, 0x60, 0, 1], 0), None);
        assert_eq!(
            instruction_len(&[0xaa, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 2, 0, 0, 0, 1], 0,),
            None,
        );
        assert_eq!(
            instruction_len(&[0xab, 0, 0, 0, 0, 0, 0, 0xff, 0xff, 0xff, 0xff], 0),
            None
        );
    }
}
