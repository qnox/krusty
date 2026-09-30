//! Unparenthesized `receiver?.name[indices]` is one safe selector.
//!
//! The member and its `get`/`set`/`inc` are selected on the non-null member. Parentheses are
//! handled by the parser, which reopens that form as an ordinary index of the nullable safe call.
//! Checked FIR spills the receiver; this module does not invent a source name for it.

use super::*;

pub(super) struct SafeIndexIncDec {
    pub(super) expression: ExprId,
    pub(super) receiver: ExprId,
    pub(super) access: ExprId,
    pub(super) element: ExprId,
    pub(super) indices: Vec<ExprId>,
    pub(super) updated: ExprId,
    pub(super) dec: bool,
    pub(super) prefix: bool,
}

impl Checker<'_> {
    fn open_safe_selector_scope<'a>(
        &mut self,
        scope: &'a CheckerScope<'a>,
        receiver: ExprId,
        receiver_ty: Ty,
    ) -> CheckerScope<'a> {
        let safe_receiver = receiver_ty.non_null().definitely_non_null();
        let selector_scope = scope.child(ScopeKind::Block);
        if let Some(path) = self.expr_access_path(receiver).filter(|path| {
            self.stable_path_ty(scope, path, self.span(receiver))
                .is_some()
        }) {
            self.apply_narrowing_unchecked(&selector_scope, &path, safe_receiver);
            self.apply_safe_call_origin_narrowing(&selector_scope, &path, self.span(receiver));
        }
        selector_scope
    }

    fn safe_index_member(
        &mut self,
        scope: &CheckerScope<'_>,
        access: ExprId,
        receiver_ty: Ty,
    ) -> Ty {
        let Expr::Member { name, .. } = self.file.expr(access).clone() else {
            unreachable!("a safe index selector is a member access");
        };
        let safe_receiver = receiver_ty.non_null().definitely_non_null();
        let member_ty =
            self.check_member(scope, safe_receiver, &name, self.span(access), Some(access));
        self.set(access, member_ty);
        member_ty
    }

    fn with_prepared_index_receiver<T>(
        &mut self,
        access: ExprId,
        member_ty: Ty,
        body: impl FnOnce(&mut Self) -> T,
    ) -> T {
        let saved = self.prepared_index_receiver;
        self.prepared_index_receiver = Some((access, member_ty));
        let result = body(self);
        self.prepared_index_receiver = saved;
        result
    }

    pub(super) fn expr_inner_safe_index(
        &mut self,
        scope: &CheckerScope<'_>,
        expression: ExprId,
        receiver: ExprId,
        access: ExprId,
        element: ExprId,
    ) -> Ty {
        let receiver_ty = self.expr(scope, receiver);
        if receiver_ty == Ty::Error {
            return self.set(expression, Ty::Error);
        }
        let selector_scope = self.open_safe_selector_scope(scope, receiver, receiver_ty);
        let member_ty = self.safe_index_member(&selector_scope, access, receiver_ty);
        if member_ty == Ty::Error {
            return self.set(expression, Ty::Error);
        }
        let element_ty = self.with_prepared_index_receiver(access, member_ty, |checker| {
            checker.expr(&selector_scope, element)
        });
        if element_ty == Ty::Error {
            return self.set(expression, Ty::Error);
        }
        self.set(expression, Ty::nullable(element_ty))
    }

    pub(super) fn expr_inner_safe_index_inc_dec(
        &mut self,
        scope: &CheckerScope<'_>,
        update: SafeIndexIncDec,
    ) -> Ty {
        let SafeIndexIncDec {
            expression,
            receiver,
            access,
            element,
            indices,
            updated,
            dec,
            prefix,
        } = update;
        let receiver_ty = self.expr(scope, receiver);
        if receiver_ty == Ty::Error {
            return self.set(expression, Ty::Error);
        }
        let selector_scope = self.open_safe_selector_scope(scope, receiver, receiver_ty);
        let member_ty = self.safe_index_member(&selector_scope, access, receiver_ty);
        if member_ty == Ty::Error {
            return self.set(expression, Ty::Error);
        }
        let element_ty = self.with_prepared_index_receiver(access, member_ty, |checker| {
            checker.expr(&selector_scope, element)
        });
        if element_ty == Ty::Error {
            return self.set(expression, Ty::Error);
        }
        let Some(resolution) = self.select_inc_dec(
            &selector_scope,
            IncDecSelection {
                site: IncDecSite::Expression(expression),
                decrement: dec,
                prefix,
            },
            element_ty,
            element_ty,
            false,
            self.span(expression),
        ) else {
            return self.set(expression, Ty::Error);
        };
        self.set(updated, resolution.updated_ty);
        let index_tys = indices
            .iter()
            .map(|index| self.expr_types[index.0 as usize])
            .collect::<Vec<_>>();
        let mut argument_tys = index_tys;
        argument_tys.push(resolution.updated_ty);
        let mut arguments = indices;
        arguments.push(updated);
        let Some((_, call)) = self.operator_call_ret(
            &selector_scope,
            access,
            member_ty,
            "set",
            &argument_tys,
            &arguments,
            self.span(expression),
            None,
        ) else {
            return self.set(expression, Ty::Error);
        };
        self.resolved_operator_calls
            .insert((expression, SyntheticOperatorCall::Set), call);
        self.set(expression, Ty::nullable(resolution.result_ty))
    }

    pub(super) fn try_safe_member_in_place_assignment(
        &mut self,
        scope: &CheckerScope<'_>,
        statement: StmtId,
        receiver: ExprId,
        value: ExprId,
    ) -> bool {
        let receiver_ty = self.expr(scope, receiver);
        if receiver_ty == Ty::Error {
            return false;
        }
        let selector_scope = self.open_safe_selector_scope(scope, receiver, receiver_ty);
        self.try_in_place_assignment(&selector_scope, statement, value)
    }

    pub(super) fn stmt_assign_safe_index(
        &mut self,
        scope: &CheckerScope<'_>,
        statement: StmtId,
        receiver: ExprId,
        access: ExprId,
        indices: Vec<ExprId>,
        value: ExprId,
    ) {
        let receiver_ty = self.expr(scope, receiver);
        if receiver_ty == Ty::Error {
            return;
        }
        let selector_scope = self.open_safe_selector_scope(scope, receiver, receiver_ty);
        let member_ty = self.safe_index_member(&selector_scope, access, receiver_ty);
        if member_ty == Ty::Error {
            return;
        }
        self.with_prepared_index_receiver(access, member_ty, |checker| {
            checker.stmt_assign_index(&selector_scope, statement, access, indices, value);
        });
    }
}
