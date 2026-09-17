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

impl Emitter<'_> {
    pub(super) fn mark_dispatch_line(&self, expression: ExprId, code: &mut CodeBuilder) {
        mark_expression_start(self.ir, expression, code);
    }
}
