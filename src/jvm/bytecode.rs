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
    /// An opening marker whose stack slots were reserved in the emitter before the inline
    /// lambda's frame marker was allocated. It is an internal transport distinction: both forms
    /// decode to stack-normalization boundaries and are removed before the class is written.
    BeforeInlineCallWithReservedLocals = 4,
}

impl CodegenMarker {
    pub(crate) const OWNER: &'static str = "kotlin/jvm/internal/InlineMarker";

    pub(crate) fn from_operand(operand: u8) -> Option<CodegenMarker> {
        Some(match operand {
            1 => CodegenMarker::Mark,
            2 => CodegenMarker::BeforeInlineCall,
            3 => CodegenMarker::AfterInlineCall,
            4 => CodegenMarker::BeforeInlineCallWithReservedLocals,
            _ => return None,
        })
    }

    /// The `InlineMarker` method's name and descriptor.
    pub(crate) fn method(self) -> (&'static str, &'static str) {
        match self {
            CodegenMarker::Mark => ("mark", "(I)V"),
            CodegenMarker::BeforeInlineCall => ("beforeInlineCall", "()V"),
            CodegenMarker::AfterInlineCall => ("afterInlineCall", "()V"),
            CodegenMarker::BeforeInlineCallWithReservedLocals => {
                ("beforeInlineCallWithReservedLocals", "()V")
            }
        }
    }
}

/// One side of an `InlineMarker.beforeInlineCall`/`afterInlineCall` bracket.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum InlineCallBracket {
    Open,
    Close,
}

impl InlineCallBracket {
    /// The bracket side a decoded instruction is, if it is one.
    pub(crate) fn of_insn(insn: &crate::jvm::inline::Insn) -> Option<InlineCallBracket> {
        let crate::jvm::inline::Insn::Plain { op, operands } = insn else {
            return None;
        };
        if *op != CODEGEN_MARKER_OP {
            return None;
        }
        match CodegenMarker::from_operand(*operands.first()?)? {
            CodegenMarker::BeforeInlineCall | CodegenMarker::BeforeInlineCallWithReservedLocals => {
                Some(InlineCallBracket::Open)
            }
            CodegenMarker::AfterInlineCall => Some(InlineCallBracket::Close),
            CodegenMarker::Mark => None,
        }
    }
}

/// A closing `afterInlineCall` with no `beforeInlineCall` opening it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct UnpairedInlineCallMarker;

/// The `beforeInlineCall`/`afterInlineCall` brackets of a body, followed as kotlinc's FixStack
/// (`FixStackAnalyzer`) rewrites them when the class is written: the opening marker saves the
/// operand stack into locals and clears it, the closing one reloads the saved values under the
/// bracketed code's result. Every analysis that reads a body still carrying the markers (the frame
/// computation, the coroutine machine's frame typing, the method-node stack shapes) follows them
/// through this one model, so the states they compute are those of the rewritten body, which is
/// what the class file carries.
#[derive(Clone, Debug, Default)]
pub(crate) struct InlineCallBrackets {
    /// By instruction: which side of a bracket it is.
    sides: Vec<Option<InlineCallBracket>>,
    /// By closing instruction: the opening one, by nesting (`FixStackContext`).
    opening: Vec<Option<usize>>,
}

impl InlineCallBrackets {
    /// Pair the brackets of a body given each instruction's side.
    pub(crate) fn pair(
        sides: impl IntoIterator<Item = Option<InlineCallBracket>>,
    ) -> InlineCallBrackets {
        let sides: Vec<_> = sides.into_iter().collect();
        let mut opening = vec![None; sides.len()];
        let mut open = Vec::new();
        for (index, side) in sides.iter().enumerate() {
            match side {
                Some(InlineCallBracket::Open) => open.push(index),
                Some(InlineCallBracket::Close) => opening[index] = open.pop(),
                None => {}
            }
        }
        InlineCallBrackets { sides, opening }
    }

    /// The brackets of a decoded body.
    pub(crate) fn of_insns(insns: &[crate::jvm::inline::Insn]) -> InlineCallBrackets {
        InlineCallBrackets::pair(insns.iter().map(InlineCallBracket::of_insn))
    }

    /// Follow the instruction at `index` on `stack`, the stack it left (a marker itself is
    /// stack-neutral). An opening marker clears the stack and returns what it saved; a closing one
    /// puts `before_opening(opening)` — the stack before its opening marker — back under the
    /// bracketed code's result. Anything else leaves the stack alone.
    pub(crate) fn follow<T>(
        &self,
        index: usize,
        stack: &mut Vec<T>,
        before_opening: impl FnOnce(usize) -> Option<Vec<T>>,
    ) -> Result<Option<Vec<T>>, UnpairedInlineCallMarker> {
        match self.sides.get(index).copied().flatten() {
            None => Ok(None),
            Some(InlineCallBracket::Open) => Ok(Some(std::mem::take(stack))),
            Some(InlineCallBracket::Close) => {
                let under = self.opening[index]
                    .and_then(before_opening)
                    .ok_or(UnpairedInlineCallMarker)?;
                // The bracketed code's result, normally one value or none; a body that left more
                // is FixStack's to reject, so its values are kept.
                let inner = std::mem::replace(stack, under);
                stack.extend(inner);
                Ok(None)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::instruction_len;
    use super::{InlineCallBracket, InlineCallBrackets, UnpairedInlineCallMarker};

    #[test]
    fn a_bracket_saves_the_stack_and_puts_it_back_under_the_result() {
        use InlineCallBracket::{Close, Open};
        // 0: push ; 1: open ; 2: push ; 3: open ; 4: push ; 5: close ; 6: close ; 7: close
        let brackets = InlineCallBrackets::pair([
            None,
            Some(Open),
            None,
            Some(Open),
            None,
            Some(Close),
            Some(Close),
            Some(Close),
        ]);
        let before = [
            vec![],
            vec!["dest"],
            vec![],
            vec!["inner"],
            vec![],
            vec!["x"],
            vec!["inner", "x"],
        ];
        let mut stack = vec!["dest"];
        assert_eq!(brackets.follow(0, &mut stack, |_| None), Ok(None));
        assert_eq!(
            brackets.follow(1, &mut stack, |_| None),
            Ok(Some(vec!["dest"]))
        );
        assert!(stack.is_empty());
        let mut inner = vec!["x"];
        // The inner closing marker reloads what its own opening saved.
        assert_eq!(
            brackets.follow(5, &mut inner, |opening| Some(before[opening].clone())),
            Ok(None)
        );
        assert_eq!(inner, ["inner", "x"]);
        let mut result = vec!["r"];
        assert_eq!(
            brackets.follow(6, &mut result, |opening| Some(before[opening].clone())),
            Ok(None)
        );
        assert_eq!(result, ["dest", "r"]);
        assert_eq!(
            brackets.follow(7, &mut result, |opening| Some(before[opening].clone())),
            Err(UnpairedInlineCallMarker)
        );
    }

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
