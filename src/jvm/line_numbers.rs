//! kotlinc's JVM line-number rule, by IR node kind.
//!
//! kotlinc's `ExpressionCodegen` decides every `LineNumberTable` entry from the kind of the IR
//! element it visits and that element's offsets; lowering gave every element the offsets of the
//! FIR element that built it. This module is that decision for krusty's IR: which nodes mark
//! their own line where their code starts, before their operands, and which mark it only at the
//! instruction that completes them (or not at all). Emission asks here instead of marking per
//! construct.
//!
//! The kotlinc sites (2.4.21, `compiler/ir/backend.jvm/codegen/.../ExpressionCodegen.kt`):
//!
//! - before operands: `visitGetValue`, `visitConst`, `visitFieldAccess`, `visitWhen`, `visitThrow`,
//!   `visitTry`, `visitBreakContinue`, `visitConstructorCall`, an intrinsic call and a suspend
//!   call;
//! - after operands: an ordinary call before its `invoke`, `visitReturn`, `visitVariable` and
//!   `visitSetValue` before their store;
//! - never at their start: blocks, type operators and loops, whose first line is the first line an
//!   inner node marks.
//!
//! A string concatenation is realized as kotlinc's `JvmStringConcatenationLowering` builds it: a
//! `StringBuilder` constructor call at the concatenation's offsets, which marks its start.
//!
//! Whether a mark writes an entry is [`LineNumberMapper`]'s decision, kotlinc's
//! `LineNumberMapper`: a mark of the line it marked last writes nothing, an inlined call makes it
//! forget that line (or, inside a condition, registers it again at once), and every other mark
//! appends an entry, a second one at the same offset included.

mod intrinsic_calls;

pub(crate) use intrinsic_calls::{inversion_mask_line, states_line_before_operands};

use crate::fir::SyntheticOriginKind;
use crate::ir::{ExprId, IrExpr, IrFile, IrNodeOrigin, IrTypeOp, NullCheck};

/// kotlinc's `LineNumberMapper` for one method body: the line it marked last and whether marks
/// are suppressed.
#[derive(Clone, Default)]
pub(crate) struct LineNumberMapper {
    /// `lastLineNumber`; `None` is kotlinc's `-1`, before the first mark and after an inlined
    /// call.
    last_line: Option<u16>,
    /// Open `noLineNumberScope`s.
    no_line_scopes: u32,
}

impl LineNumberMapper {
    /// `markLineNumber`: whether `line` is due an entry, which is when it is not the line marked
    /// last and no scope suppresses marks. A due line becomes the line marked last.
    pub(crate) fn mark(&mut self, line: u16) -> bool {
        if self.no_line_scopes > 0 || self.last_line == Some(line) {
            return false;
        }
        self.last_line = Some(line);
        true
    }

    /// `markLineNumberAfterInlineIfNeeded`: the line to register again at once, inside a condition
    /// or a no-line scope; elsewhere the mapper forgets its line, so the next mark of any line,
    /// the same one included, writes an entry.
    pub(crate) fn after_inline(&mut self, register: bool) -> Option<u16> {
        if register || self.no_line_scopes > 0 {
            self.last_line
        } else {
            self.last_line = None;
            None
        }
    }

    /// `lastLineNumber`.
    pub(crate) fn last_line(&self) -> Option<u16> {
        self.last_line
    }

    /// Open a `noLineNumberScope`: marks write nothing until it is closed.
    pub(crate) fn open_no_line_scope(&mut self) {
        self.no_line_scopes += 1;
    }

    /// Close the innermost `noLineNumberScope`.
    pub(crate) fn close_no_line_scope(&mut self) {
        self.no_line_scopes = self.no_line_scopes.saturating_sub(1);
    }
}

/// Whether kotlinc marks `expression`'s line where its code starts, before its operands.
///
/// A node that marks only after its operands (a call at its `invoke`, a `return`, a store) or not
/// at all (a block, a cast, a concatenation, a loop) leaves its first line to the first operand
/// that marks one, as kotlinc's visitor does.
pub(crate) fn marks_line_at_start(ir: &IrFile, expression: ExprId) -> bool {
    // An inline call marks its line before the inliner copies the callee's body (kotlinc's
    // `visitCall` mark ahead of `IrInlineCodegen.genCall`): the block realizing it in place starts
    // with that mark.
    if realizes_inlined_call(ir, expression) {
        return true;
    }
    // `suspendCoroutineUninterceptedOrReturn` realized in place is a suspend call, which kotlinc
    // marks where it starts, ahead of the operand it passes the block.
    if ir.is_unintercepted_suspension(expression) {
        return true;
    }
    match ir.expr(expression) {
        // `E.values()` and `E.entries` are calls without operands: their invoke is where they
        // start, so they mark there like any node that marks before its operands.
        IrExpr::Call { .. }
        | IrExpr::MethodCall { .. }
        | IrExpr::InvokeFunction { .. }
        | IrExpr::EnumValueOf { .. } => ir.suspend_calls.contains_key(&expression),
        // A block that opens an inline frame is an inlined call realized in place, a lambda's
        // invocation included: the call marks its line ahead of the body its frame marker opens.
        // An unsigned bitwise operation is a call of the standard library's `@InlineOnly` member
        // over the signed carrier, which marks once its arguments are generated; a signed one is
        // an intrinsic, which marks before them.
        IrExpr::PrimitiveBinOp { .. } => !has_unsigned_result(ir, expression),
        // A block of no statements realizing an invocation in place around such a block marks
        // the invocation's line there.
        IrExpr::Block { stmts, value } => match stmts.first() {
            Some(&first) => matches!(ir.expr(first), IrExpr::InlineFrameMarker),
            None => value.is_some_and(|value| opens_inline_frame(ir, value)),
        },
        // A property read or write is realized as a call of its accessor, which marks at its
        // invoke; one realized as a field access marks where its realization is chosen, as
        // kotlinc's `visitFieldAccess` does before the receiver.
        IrExpr::PropertyRead { .. }
        | IrExpr::PropertyWrite { .. }
        | IrExpr::Return(_)
        | IrExpr::Variable { .. }
        | IrExpr::SetValue { .. }
        | IrExpr::TypeOp { .. }
        | IrExpr::ReifiedTypeOp { .. }
        | IrExpr::While { .. }
        | IrExpr::BottomValue { .. }
        | IrExpr::InlineFrameMarker => false,
        _ => true,
    }
}

/// Whether `expression` states its own line where its code starts even where it is a statement or
/// an operand whose own kind marks nothing there:
///
/// - a source `!!`, kotlinc's `CHECK_NOT_NULL` intrinsic call, which marks before its operand; an
///   implicit coercion of its result is a type operator, which marks nothing of its own;
/// - an elvis, which kotlinc's `visitBlock` of the `ELVIS` block marks where it starts (the
///   `when` it folds to marks the same line again, which writes nothing). A later link of an elvis
///   chain is folded into the chain's block and opens none.
pub(crate) fn states_line_at_start(ir: &IrFile, expression: ExprId) -> bool {
    match ir.expr(expression) {
        IrExpr::NotNullAssert {
            check: NullCheck::Source,
            ..
        } => true,
        IrExpr::TypeOp {
            op: IrTypeOp::ImplicitCoercion,
            arg,
            ..
        } => states_line_at_start(ir, *arg),
        IrExpr::Block {
            value: Some(value), ..
        } => ir.whens.elvises.contains_key(value) && !ir.whens.elvis_chain_links.contains(value),
        _ => false,
    }
}

/// Whether `expression` marks its own line before its operands wherever it is emitted, a statement
/// included: an intrinsic or safe call ([`states_line_before_operands`]) or a node whose kind
/// kotlinc marks so ([`states_line_at_start`]).
pub(crate) fn marks_line_before_operands(ir: &IrFile, expression: ExprId) -> bool {
    states_line_before_operands(ir, expression) || states_line_at_start(ir, expression)
}

/// Whether a value `expression` marks its line where its code starts: its kind marks there
/// ([`marks_line_at_start`]), or it marks before its operands wherever it is emitted.
pub(crate) fn marks_value_line_at_start(ir: &IrFile, expression: ExprId) -> bool {
    marks_line_at_start(ir, expression) || marks_line_before_operands(ir, expression)
}

/// Whether `expression`'s line is one the inliner copies with an inline function's compiled body
/// rather than a position of the caller's source: a line of an external inline declaration's own
/// body, or the synthetic line an `@InlineOnly` lambda marker takes. kotlinc's inliner copies such
/// a `LineNumberNode` in front of the instruction it numbers, so it marks where the node starts.
pub(crate) fn is_copied_inline_line(ir: &IrFile, expression: ExprId) -> bool {
    ir.external_frame_lines.contains_key(&expression)
        || ir.inline_synthetic_lines.contains(&expression)
}

/// Whether `expression` realizes an inlined call in place (`SyntheticOriginKind::InlinedCall`) or
/// is an external-inline expansion lowered from its declaration plan.
pub(crate) fn realizes_inlined_call(ir: &IrFile, expression: ExprId) -> bool {
    ir.external_inline_expansions.contains(&expression)
        || matches!(
            ir.fir_origins.get(&expression),
            Some(IrNodeOrigin::Synthetic {
                kind: SyntheticOriginKind::InlinedCall,
                ..
            })
        )
}

/// Whether `expression` is a block whose first statement opens an inline frame.
fn opens_inline_frame(ir: &IrFile, expression: ExprId) -> bool {
    matches!(ir.expr(expression), IrExpr::Block { stmts, .. }
        if stmts.first().is_some_and(|&first| matches!(ir.expr(first), IrExpr::InlineFrameMarker)))
}

/// Whether `expression`'s checked result is an unsigned integer.
fn has_unsigned_result(ir: &IrFile, expression: ExprId) -> bool {
    ir.logical_types
        .get(&expression)
        .is_some_and(|ty| ty.canonical_semantic().non_null().is_unsigned())
}

#[cfg(test)]
mod tests {
    use super::LineNumberMapper;

    #[test]
    fn a_mark_of_the_line_marked_last_writes_nothing() {
        let mut mapper = LineNumberMapper::default();
        assert!(mapper.mark(3));
        assert!(!mapper.mark(3));
        assert!(mapper.mark(4));
        assert!(mapper.mark(3));
    }

    #[test]
    fn an_inlined_call_forgets_the_line_outside_a_condition() {
        let mut mapper = LineNumberMapper::default();
        assert!(mapper.mark(7));
        assert_eq!(mapper.after_inline(false), None);
        assert_eq!(mapper.last_line(), None);
        assert!(mapper.mark(7));
    }

    #[test]
    fn an_inlined_call_inside_a_condition_registers_the_line_again() {
        let mut mapper = LineNumberMapper::default();
        assert!(mapper.mark(7));
        assert_eq!(mapper.after_inline(true), Some(7));
        assert!(!mapper.mark(7));
    }

    #[test]
    fn a_no_line_scope_writes_nothing_and_keeps_the_line() {
        let mut mapper = LineNumberMapper::default();
        assert!(mapper.mark(2));
        mapper.open_no_line_scope();
        assert!(!mapper.mark(5));
        mapper.close_no_line_scope();
        assert_eq!(mapper.last_line(), Some(2));
        assert!(mapper.mark(5));
    }
}
