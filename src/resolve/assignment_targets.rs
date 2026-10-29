//! Assignment targets: the spans an assignment's diagnostics point at, the rejection of a `val`
//! reassignment, and kotlinc's assignment plugin, which makes an assignment to a `val` of an
//! annotated type the call `target.assign(value)`.

use super::*;

/// The function an overloaded assignment calls.
pub(super) const ASSIGNMENT_CONVENTION: &str = "assign";

const NO_APPLICABLE_ASSIGN: &str = "no applicable 'assign' function found for '=' overload.";

const ASSIGNMENT_RESULT_NOT_UNIT: &str =
    "function 'assign' used for '=' overload must return 'Unit'.";

impl Checker<'_> {
    /// kotlinc's assignment plugin rejects a declared `assign(value)` that an overloaded assignment
    /// could call, on a `receiver` of an annotated type, when its result is not `Unit`: at the
    /// written result type, or at the name when the result is inferred.
    pub(super) fn check_assignment_declaration(
        &mut self,
        function: &FunDecl,
        result: Ty,
        receiver: Option<Ty>,
    ) {
        if function.name != ASSIGNMENT_CONVENTION
            || function.params.len() - function.context_count != 1
            || matches!(result, Ty::Unit | Ty::Error)
            || !receiver
                .is_some_and(|receiver| self.resolver().supports_assignment_overloading(receiver))
        {
            return;
        }
        let span = function
            .ret
            .as_ref()
            .map_or(function.name_span, |result| result.span);
        self.diags.error(span, ASSIGNMENT_RESULT_NOT_UNIT);
    }

    pub(super) fn assignment_target_span(&self, statement: StmtId) -> Span {
        self.file
            .assignment_target_spans
            .get(&statement.0)
            .copied()
            .unwrap_or(self.file.stmt_spans[statement.0 as usize])
    }

    pub(super) fn assignment_member_name_span(&self, statement: StmtId, name: &str) -> Span {
        let span = self.assignment_target_span(statement);
        Span::new(span.hi.saturating_sub(name.len() as u32), span.hi)
    }

    pub(super) fn report_val_reassignment(&mut self, span: Span, message: impl Into<String>) {
        if self.unreachable_statement_depth != 0 {
            return;
        }
        self.diags
            .error_kind(span, crate::diag::DiagnosticKind::ValReassignment, message);
    }

    /// kotlinc's assignment plugin. `target = value`, where `target` is a non-local `val` property
    /// or a final Java field of type `target_ty` whose class or supertype carries an assignment
    /// annotation, is the call `target.assign(value)` the parser kept beside the statement. It is
    /// resolved and reported like any other call; when it fails kotlinc adds its own error at the
    /// `=`, and a selected call that does not return `Unit` is rejected there. Returns whether the
    /// plugin took the statement over.
    pub(super) fn try_overloaded_assignment(
        &mut self,
        scope: &CheckerScope<'_>,
        statement: StmtId,
        target_ty: Ty,
        value: ExprId,
    ) -> bool {
        if !self.resolver().supports_assignment_overloading(target_ty) {
            return false;
        }
        let Some(&call) = self.file.assignment_overload_calls.get(&statement.0) else {
            return false;
        };
        let operator = self.value_operator_span(value);
        let checkpoint = self.diags.diags.len();
        let result = self.expr(scope, call);
        let failed = result == Ty::Error
            || self.diags.diags[checkpoint..]
                .iter()
                .any(|diagnostic| diagnostic.severity == crate::diag::Severity::Error);
        if failed {
            self.diags.error(operator, NO_APPLICABLE_ASSIGN);
        } else if result != Ty::Unit {
            self.diags.error(operator, ASSIGNMENT_RESULT_NOT_UNIT);
        } else {
            self.stmt_lowers
                .insert(statement, StmtLowering::OverloadedAssignment { call });
        }
        true
    }
}
