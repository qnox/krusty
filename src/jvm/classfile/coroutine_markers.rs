//! Positions inside a method's bytecode that must survive being relocated into a spliced inline
//! body.
//!
//! A coroutine state machine has to bind labels at two places inside the body it wraps: where a
//! suspension's spill block goes, and where the resume path rejoins after the call. When the
//! suspension sits inside a lambda that will be spliced into a classpath inline body, neither
//! position is known while that lambda is being emitted — the splice decides where its instructions
//! land. A byte offset recorded beforehand is therefore worthless, but an *instruction* travels with
//! the code through every relocation.
//!
//! So the emitter writes a marker instruction at each position and looks for it once the method's
//! bytes are final. `impdep1` (`0xfe`) is reserved by JVMS §6.2 for an implementation's own use and
//! no compiler emits it, which is exactly the guarantee needed. Every marker is overwritten with
//! `nop`s the moment its label is bound, so none can reach a class file; [`Self::marker_positions`]
//! and [`Self::erase_markers`] are always used as a pair.

use super::CodeBuilder;

/// What a marker denotes. The kind travels in the marker's first operand byte.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CoroutineMarker {
    /// Immediately before a suspension's operands: where the spill block and the `label` store go.
    Suspension = 1,
    /// Immediately after a suspension's `COROUTINE_SUSPENDED` check: where the resume path rejoins
    /// with the call's result on the operand stack.
    Resume = 2,
}

/// The opcode. `impdep1`; see the module docs.
const MARKER_OP: u8 = 0xfe;
/// Opcode + kind + a two-byte ordinal.
pub const MARKER_LEN: usize = 4;

impl CodeBuilder {
    /// Emit a marker for suspension `ordinal`. Stack-neutral, like the `nop`s that replace it.
    pub fn coroutine_marker(&mut self, kind: CoroutineMarker, ordinal: u16) {
        if !self.is_dead() {
            self.bytes.push(MARKER_OP);
            self.bytes.push(kind as u8);
            self.bytes.push((ordinal >> 8) as u8);
            self.bytes.push((ordinal & 0xff) as u8);
        }
    }

    /// Every marker in the finished bytes: `(byte offset, kind, ordinal)`, in offset order.
    ///
    /// The scan walks instruction by instruction rather than searching for the opcode byte, because
    /// `0xfe` also occurs inside operands — a constant-pool index, a branch offset, a `bipush`.
    pub fn marker_positions(&self) -> Option<Vec<(usize, CoroutineMarker, u16)>> {
        let mut found = Vec::new();
        let mut pc = 0;
        while pc < self.bytes.len() {
            let len = crate::jvm::inline::instruction_len(&self.bytes, pc)?;
            if self.bytes[pc] == MARKER_OP {
                let kind = match self.bytes.get(pc + 1)? {
                    1 => CoroutineMarker::Suspension,
                    2 => CoroutineMarker::Resume,
                    _ => return None,
                };
                let ordinal = (u16::from(*self.bytes.get(pc + 2)?) << 8)
                    | u16::from(*self.bytes.get(pc + 3)?);
                found.push((pc, kind, ordinal));
            }
            pc += len;
        }
        Some(found)
    }

    /// Overwrite every marker with `nop`s, keeping all offsets. Call once the labels are bound.
    pub fn erase_markers(&mut self) -> Option<()> {
        for (offset, _, _) in self.marker_positions()? {
            for byte in &mut self.bytes[offset..offset + MARKER_LEN] {
                *byte = 0x00;
            }
        }
        Some(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_marker_is_found_by_its_position_and_erased_in_place() {
        let mut code = CodeBuilder::new(1);
        code.nop();
        code.coroutine_marker(CoroutineMarker::Suspension, 0);
        code.nop();
        code.coroutine_marker(CoroutineMarker::Resume, 513);
        code.ret_void();
        let before = code.bytes.len();
        assert_eq!(
            code.marker_positions().expect("scan"),
            [
                (1, CoroutineMarker::Suspension, 0),
                (6, CoroutineMarker::Resume, 513),
            ]
        );
        code.erase_markers().expect("erase");
        assert_eq!(code.bytes.len(), before, "erasing must not move any offset");
        assert!(code.marker_positions().expect("rescan").is_empty());
        assert!(code.bytes.iter().all(|&byte| byte != 0xfe));
    }

    /// `0xfe` occurs constantly inside operands. A byte scan would report those as markers; an
    /// instruction walk cannot.
    #[test]
    fn an_operand_byte_that_happens_to_be_the_marker_opcode_is_not_a_marker() {
        let mut code = CodeBuilder::new(1);
        code.bytes.extend_from_slice(&[0x10, 0xfe]); // bipush -2
        code.bytes.extend_from_slice(&[0x11, 0xfe, 0xfe]); // sipush
        code.nop();
        assert!(code.marker_positions().expect("scan").is_empty());
    }
}
