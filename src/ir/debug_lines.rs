//! Source lines a generated expression maps to only at one physical point rather than at its
//! start. Common lowering records the semantic fact once; a backend asks where the line belongs
//! instead of inferring it from a synthetic origin or expression shape.

use std::collections::HashMap;

use super::{ExprId, IrFile};

#[derive(Default)]
pub(super) struct GeneratedLineMarks {
    /// Implicit return identity → the expression body's closing source line.
    ///
    /// An explicit `return expression` keeps the call/return line already in effect. An
    /// expression-bodied callable instead maps its generated return instruction to the end of the
    /// body expression.
    implicit_return_ends: HashMap<ExprId, u32>,
    /// Line a generated call enters only where it dispatches: its operands carry no source
    /// position of their own, so nothing marks the line at its start (a callable-reference
    /// carrier's `invoke`, whose stored receiver is read ahead of the call's line).
    dispatches: HashMap<ExprId, u32>,
}

impl IrFile {
    pub(crate) fn mark_implicit_return_end_line(&mut self, returned: ExprId, line: u32) {
        self.generated_lines
            .implicit_return_ends
            .insert(returned, line);
    }

    pub(crate) fn implicit_return_end_line(&self, returned: ExprId) -> Option<u32> {
        self.generated_lines
            .implicit_return_ends
            .get(&returned)
            .copied()
    }

    pub(crate) fn mark_dispatch_line(&mut self, call: ExprId, line: u32) {
        self.generated_lines.dispatches.insert(call, line);
    }

    pub(crate) fn dispatch_line(&self, call: ExprId) -> Option<u32> {
        self.generated_lines.dispatches.get(&call).copied()
    }

    /// Carry `source`'s generated line marks over to its copy `target`.
    pub(super) fn copy_generated_line_marks(&mut self, source: ExprId, target: ExprId) {
        let marks = &mut self.generated_lines;
        if let Some(&line) = marks.implicit_return_ends.get(&source) {
            marks.implicit_return_ends.insert(target, line);
        }
        if let Some(&line) = marks.dispatches.get(&source) {
            marks.dispatches.insert(target, line);
        }
    }
}
