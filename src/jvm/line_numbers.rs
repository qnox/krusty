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

use crate::fir::SyntheticOriginKind;
use crate::ir::{ExprId, IrExpr, IrFile, IrNodeOrigin};

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
    match ir.expr(expression) {
        // `E.values()` and `E.entries` are calls without operands: their invoke is where they
        // start, so they mark there like any node that marks before its operands.
        IrExpr::Call { .. }
        | IrExpr::MethodCall { .. }
        | IrExpr::InvokeFunction { .. }
        | IrExpr::EnumValueOf { .. } => ir.suspend_calls.contains_key(&expression),
        // A block that opens an inline frame is an inlined call realized in place, a lambda's
        // invocation included: the call marks its line ahead of the body its frame marker opens.
        IrExpr::Block { stmts, .. } => stmts
            .first()
            .is_some_and(|&first| matches!(ir.expr(first), IrExpr::InlineFrameMarker)),
        IrExpr::Return(_)
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
