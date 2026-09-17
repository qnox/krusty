//! Source-line attribution at common-IR expression boundaries.
//!
//! Common lowering owns source locations. This module is the JVM debug-info boundary that turns
//! those locations into `LineNumberTable` marks without teaching expression emission how the maps
//! are represented.

use crate::ir::{ExprId, IrFile};
use crate::jvm::classfile::CodeBuilder;

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
