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
/// to the pending return.
pub(super) fn mark_return(ir: &IrFile, returned: ExprId, code: &mut CodeBuilder) {
    if let Some(&line) = ir
        .implicit_return_end_lines
        .get(&returned)
        .or_else(|| ir.expr_source_lines.get(&returned))
    {
        if line != 0 {
            code.mark_line(line);
        }
    }
}

impl Emitter<'_> {
    pub(super) fn mark_dispatch_line(&self, expression: ExprId, code: &mut CodeBuilder) {
        mark_expression_start(self.ir, expression, code);
    }
}
