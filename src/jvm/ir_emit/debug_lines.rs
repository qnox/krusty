//! Source-line attribution at common-IR expression boundaries.
//!
//! Common lowering owns source locations. This module is the JVM debug-info boundary that turns
//! those locations into `LineNumberTable` marks without teaching expression emission how the maps
//! are represented.

use crate::ir::{ExprId, IrFile};
use crate::jvm::classfile::CodeBuilder;

use super::Emitter;

/// Mark a source statement or block value at the first instruction it emits.
pub(super) fn mark_statement(ir: &IrFile, expression: ExprId, code: &mut CodeBuilder) {
    if let Some(&line) = ir.expr_lines.get(&expression) {
        code.mark_line(line);
    }
}

/// Mark every value expression that begins on a different source line.
///
/// Operands of a multi-line call use this path independently. `CodeBuilder::mark_line` deduplicates
/// a line already in effect and replaces a mark at the same bytecode offset.
pub(super) fn mark_expression_start(ir: &IrFile, expression: ExprId, code: &mut CodeBuilder) {
    if let Some(&line) = ir.expr_source_lines.get(&expression) {
        if line != 0 {
            code.mark_line(line);
        }
    }
}

/// Mark the actual return instruction after any active `finally` blocks have run.
///
/// An implicit expression-body return uses the body's closing line. An explicit return uses its own
/// source line, which matters when a finalizer changed the line in effect before control comes back
/// to the pending return. A `return` written as a statement carries that line in the statement map
/// rather than the per-expression one, so both are consulted.
pub(super) fn mark_return(ir: &IrFile, returned: ExprId, code: &mut CodeBuilder) {
    if let Some(&line) = ir
        .implicit_return_end_lines
        .get(&returned)
        .or_else(|| ir.expr_source_lines.get(&returned))
        .or_else(|| ir.expr_lines.get(&returned))
    {
        if line != 0 {
            code.mark_line(line);
        }
    }
}

/// Mark a loop's own line at control lowering generated for it: a `for` loop's update and exit
/// test, and the bottom condition of its `do…while` shape.
///
/// kotlinc's `ForLoopsLowering` builds that control at the loop's offsets, and its codegen marks it
/// like any other expression, so after the body the loop's line comes back at the first instruction
/// of the update (`line 7: 74` after the body's `line 8`). `None` — a loop emitted with no statement
/// line in effect — marks nothing, as a node without offsets does in kotlinc.
pub(super) fn mark_loop_control(loop_line: Option<u32>, code: &mut CodeBuilder) {
    if let Some(line) = loop_line {
        if line != 0 {
            code.mark_line(line);
        }
    }
}

/// Mark the line a block's own emission marks first.
///
/// A `finally` is emitted twice: inline on the normal path, and again in the catch-all handler. The
/// handler's entry — the `astore` that parks the in-flight exception — belongs to the finalizer it
/// introduces, so kotlinc gives it the finalizer's FIRST line rather than the `finally` keyword's.
/// Marking it before the store puts both copies of the finalizer on the same line, and the copy's
/// own leading mark then deduplicates.
pub(super) fn mark_block_entry(ir: &IrFile, block: ExprId, code: &mut CodeBuilder) {
    let mut expression = block;
    loop {
        if let Some(&line) = ir.expr_lines.get(&expression) {
            if line != 0 {
                code.mark_line(line);
            }
            return;
        }
        let crate::ir::IrExpr::Block { stmts, value } = ir.expr(expression) else {
            return;
        };
        let Some(&first) = stmts.first().or(value.as_ref()) else {
            return;
        };
        expression = first;
    }
}

/// Mark the `goto` that leaves a `try` after an inlined `finally` copy.
///
/// kotlinc gives that jump the `finally` block's CLOSING line: it belongs to the end of the
/// finalizer, not to whatever statement follows the `try`. Without it the finalizer's own first
/// line stays in effect all the way into the catch-all handler, whose identical mark then
/// deduplicates away — so one missing entry costs two.
pub(super) fn mark_block_exit(ir: &IrFile, block: ExprId, code: &mut CodeBuilder) {
    if let Some(&line) = ir.expr_end_lines.get(&block) {
        if line != 0 {
            code.mark_line(line);
        }
    }
}

impl Emitter<'_> {
    /// Run `emit` as the emission of a `when` branch condition (kotlinc's `isInsideCondition`).
    pub(super) fn in_condition<R>(&mut self, emit: impl FnOnce(&mut Self) -> R) -> R {
        let outer = std::mem::replace(&mut self.inside_condition, true);
        let result = emit(self);
        self.inside_condition = outer;
        result
    }

    /// Put a call's own line back in effect at its physical dispatch.
    ///
    /// A multi-line call's operands each mark their own line as they are pushed, so by the time the
    /// `invoke*` is reached the line in effect is the last operand's. kotlinc puts the call's line
    /// back at the dispatch — but only where the instruction really is a dispatch. An operation
    /// lowered to bytecode that calls nothing keeps the operand's line, and marking it would add an
    /// entry kotlinc does not have.
    ///
    /// Which physical operations dispatch is decided per operation, at the site that selects it,
    /// and every answer is pinned by a complete-table differential in
    /// `tests/expression_line_marks_e2e.rs`:
    ///
    /// - every `IrExpr::Call` callee form, `IrExpr::New`, `IrExpr::MethodCall`;
    /// - `IrExpr::InvokeFunction` — a function value's `FunctionN.invoke`;
    /// - `IrExpr::EnumValueOf` — the member `E.valueOf` realization;
    /// - a property read or write whose accessor is a real method, not a field access;
    /// - the intrinsics whose lowering IS a call: `PrimitiveCompare`'s `Integer.compare`,
    ///   `String.get`'s `charAt`.
    ///
    /// Deliberately NOT dispatches: an array read or write, a field read or write, an arithmetic
    /// or comparison instruction, and the reified `enumValueOf<E>` template — that one is an
    /// INLINE expansion, and kotlinc marks its call SITE instead (see
    /// [`Self::mark_inline_call_site_line`]).
    pub(super) fn mark_dispatch_line(&self, expression: ExprId, code: &mut CodeBuilder) {
        mark_expression_start(self.ir, expression, code);
    }

    /// Mark the site of an INLINE call, at the first instruction its expansion emits.
    ///
    /// kotlinc keeps this entry even when the expansion's own first instruction begins on the same
    /// offset and a different line, so both source positions survive: `enumValueOf<E>(\n    s\n)`
    /// records the call's line and its argument's line at one pc. An inline expansion has no
    /// dispatch of its own to return to afterwards — the `invoke*` it ends with belongs to the
    /// inlined body, not to the call — so this REPLACES the dispatch mark rather than joining it.
    pub(super) fn mark_inline_call_site_line(&self, expression: ExprId, code: &mut CodeBuilder) {
        if let Some(&line) = self.ir.expr_source_lines.get(&expression) {
            if line != 0 {
                code.mark_line_retained(line);
            }
        }
    }
}
