//! Selection of source augmented assignments as ordinary convention calls.

use super::*;

impl Checker<'_> {
    pub(super) fn try_in_place_assignment(
        &mut self,
        scope: &CheckerScope<'_>,
        statement: StmtId,
        value: ExprId,
    ) -> bool {
        let Expr::Binary { op, lhs, rhs, .. } = self.file.expr(value).clone() else {
            return false;
        };
        self.try_in_place_assignment_operands(scope, statement, op, lhs, rhs)
    }

    pub(super) fn try_in_place_assignment_operands(
        &mut self,
        scope: &CheckerScope<'_>,
        statement: StmtId,
        operation: BinOp,
        receiver: ExprId,
        argument: ExprId,
    ) -> bool {
        let Some(name) = assign_op_name(operation) else {
            return false;
        };
        let receiver_ty = self.expr(scope, receiver);
        if receiver_ty == Ty::Error {
            return false;
        }
        // Select the convention's semantic parameter shape before checking a postponed RHS.
        // `C<(Int) -> Int> += { it }` binds an extension's `<T>` from the receiver, so typing the
        // lambda without that shape first would irreversibly publish `(Any) -> Any` and make the
        // otherwise-applicable `plusAssign(T)` look inapplicable.
        let selected_parameter = self
            .selected_operator_params(scope, receiver_ty, name, &[argument])
            .and_then(|parameters| parameters.first().copied());
        let argument_ty = match selected_parameter {
            Some(expected)
                if matches!(
                    self.file.expr(argument),
                    Expr::Lambda { .. } | Expr::CallableRef { .. }
                ) || self.call_result_can_bind_expected(argument, expected) =>
            {
                self.expr_expected(scope, argument, expected)
            }
            _ => self.expr(scope, argument),
        };
        let span = self.file.stmt_spans[statement.0 as usize];
        crate::trace_compiler!(
            "resolve",
            "in-place operator name={name} receiver={receiver_ty:?} argument={argument_ty:?}"
        );
        let Some((result, call)) = self.operator_call_ret(
            scope,
            receiver,
            receiver_ty,
            name,
            &[argument_ty],
            &[argument],
            span,
        ) else {
            crate::trace_compiler!(
                "resolve",
                "in-place operator name={name} receiver={receiver_ty:?} no target"
            );
            return false;
        };
        crate::trace_compiler!(
            "resolve",
            "in-place operator name={name} receiver={receiver_ty:?} selected={call:?} ret={result:?}"
        );
        if result == Ty::Error {
            return true;
        }
        if result != Ty::Unit {
            return false;
        }
        let capabilities = call.capabilities();
        if !matches!(
            call,
            ResolvedCall::Member(_) | ResolvedCall::Extension(_) | ResolvedCall::LocalFunction(_)
        ) {
            return false;
        }
        self.stmt_lowers.insert(
            statement,
            StmtLowering::PlusAssign(CompoundAssignmentTarget {
                call: Box::new(call),
                capabilities,
            }),
        );
        true
    }
}
