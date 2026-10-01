//! Source lines a generated expression maps to only at one physical point rather than at its
//! start. Common lowering records the semantic fact once; a backend asks where the line belongs
//! instead of inferring it from a synthetic origin or expression shape.

use std::collections::{HashMap, HashSet};

use super::{ExprId, IrFile};

#[derive(Default)]
pub(super) struct GeneratedLineMarks {
    /// Implicit return identity → the expression body's closing source line.
    ///
    /// An explicit `return expression` keeps the call/return line already in effect. An
    /// expression-bodied callable instead maps its generated return instruction to the end of the
    /// body expression.
    implicit_return_ends: HashMap<ExprId, u32>,
    /// Return a block body falls off its end into → the body's closing `}` line, which kotlinc's
    /// `setExtraLineNumberForVoidReturningFunction` marks BEFORE the returned value is loaded.
    fallthrough_returns: HashMap<ExprId, u32>,
    /// Line a generated call enters only where it dispatches: its operands carry no source
    /// position of their own, so nothing marks the line at its start (a callable-reference
    /// carrier's `invoke`, whose stored receiver is read ahead of the call's line).
    dispatches: HashMap<ExprId, u32>,
    /// Expressions kotlinc builds without source offsets (an implicit context argument): no line
    /// begins at them, so a line marked where one starts has not begun yet.
    positionless: HashSet<ExprId>,
    /// Checked operations whose source line begins at their first generated operand. A property
    /// access owns its implicit context argument even though that argument has no source position.
    generated_operand_starts: HashSet<ExprId>,
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

    pub(crate) fn mark_fallthrough_return_line(&mut self, returned: ExprId, line: u32) {
        self.generated_lines
            .fallthrough_returns
            .insert(returned, line);
    }

    pub(crate) fn fallthrough_return_line(&self, returned: ExprId) -> Option<u32> {
        self.generated_lines
            .fallthrough_returns
            .get(&returned)
            .copied()
    }

    pub(crate) fn mark_dispatch_line(&mut self, call: ExprId, line: u32) {
        self.generated_lines.dispatches.insert(call, line);
    }

    pub(crate) fn dispatch_line(&self, call: ExprId) -> Option<u32> {
        self.generated_lines.dispatches.get(&call).copied()
    }

    pub(crate) fn mark_positionless(&mut self, expression: ExprId) {
        self.generated_lines.positionless.insert(expression);
    }

    pub(crate) fn is_positionless(&self, expression: ExprId) -> bool {
        self.generated_lines.positionless.contains(&expression)
    }

    pub(crate) fn mark_generated_operand_start(&mut self, expression: ExprId) {
        self.generated_lines
            .generated_operand_starts
            .insert(expression);
    }

    pub(crate) fn starts_at_generated_operand(&self, expression: ExprId) -> bool {
        self.generated_lines
            .generated_operand_starts
            .contains(&expression)
    }

    pub(crate) fn move_generated_operand_start(&mut self, source: ExprId, target: ExprId) {
        if self
            .generated_lines
            .generated_operand_starts
            .remove(&source)
        {
            self.generated_lines.generated_operand_starts.insert(target);
        }
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
        if let Some(&line) = marks.fallthrough_returns.get(&source) {
            marks.fallthrough_returns.insert(target, line);
        }
        if marks.positionless.contains(&source) {
            marks.positionless.insert(target);
        }
        if marks.generated_operand_starts.contains(&source) {
            marks.generated_operand_starts.insert(target);
        }
    }
}
