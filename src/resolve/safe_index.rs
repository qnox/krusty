//! Unparenthesized `receiver?.name[indices]` is one safe selector.
//!
//! The member and its `get`/`set`/`inc` are selected on the non-null member. Parentheses are
//! handled by the parser, which reopens that form as an ordinary index of the nullable safe call.
//! Checked FIR spills the receiver; this module does not invent a source name for it.
//!
//! The already-selected member type is an argument of the index and compound operations. General
//! expression typing does not consult checker state to rediscover it.

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

/// Inputs of one binary expression, including a left operand already typed on a non-null receiver.
pub(super) struct BinaryCheck {
    pub(super) op: BinOp,
    pub(super) lhs: ExprId,
    pub(super) rhs: ExprId,
    pub(super) operator_span: Span,
    pub(super) prepared_lhs: Option<Ty>,
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
        self.set(access, member_ty)
    }

    fn check_expr_frame(
        &mut self,
        expression: ExprId,
        body: impl FnOnce(&mut Self, Option<Ty>) -> Ty,
    ) -> Ty {
        let expected = self.expected.take();
        let declared = std::mem::take(&mut self.expected_declared) && expected.is_some();
        self.expr_depth += 1;
        if self.expr_depth > crate::wide_stack::MAX_SEMANTIC_EXPR_DEPTH {
            self.expr_depth -= 1;
            return self.set(expression, Ty::Error);
        }
        self.extension_receiver_expr_uses[expression.0 as usize].clear();
        self.resolved_source_calls.remove(&expression);
        self.expectation_frames
            .push(conditional_branch::ExpectationFrame::new(
                expression, declared,
            ));
        let ty = crate::wide_stack::on_wide_stack(|| body(self, expected));
        self.expectation_frames.pop();
        self.expr_depth -= 1;
        if self.expr_depth == 0 {
            self.postponed_diagnostics.commit(self.diags);
        }
        ty
    }

    /// Type `element`, an index of the member, using the member type already selected.
    fn type_selected_index(
        &mut self,
        scope: &CheckerScope<'_>,
        element: ExprId,
        member_ty: Ty,
    ) -> Ty {
        let Expr::Index { array, indices } = self.file.expr(element).clone() else {
            unreachable!("a safe index element is an index expression");
        };
        self.check_expr_frame(element, |checker, _expected| {
            checker.expr_inner_index(scope, element, array, indices, Some(member_ty))
        })
    }

    fn type_compound_on_prepared_lhs(
        &mut self,
        scope: &CheckerScope<'_>,
        value: ExprId,
        lhs_ty: Ty,
        expected: Option<Ty>,
    ) -> Ty {
        let Expr::Binary {
            op,
            lhs,
            rhs,
            operator_span,
        } = self.file.expr(value).clone()
        else {
            unreachable!("a compound assignment value is a binary expression");
        };
        if let Some(expected) = expected {
            self.expected_declared |= self.block_forwards_declared_expectation();
            self.expected = Some(expected);
        }
        self.check_expr_frame(value, |checker, expected| {
            checker.expr_inner_binary(
                scope,
                value,
                BinaryCheck {
                    op,
                    lhs,
                    rhs,
                    operator_span,
                    prepared_lhs: Some(lhs_ty),
                },
                expected,
            )
        })
    }

    fn compound_index_lhs(
        &self,
        array: ExprId,
        indices: &[ExprId],
        value: ExprId,
    ) -> Option<ExprId> {
        let Expr::Binary { lhs, .. } = self.file.expr(value) else {
            return None;
        };
        match self.file.expr(*lhs) {
            Expr::Index {
                array: read_array,
                indices: read_indices,
            } if *read_array == array && read_indices == indices => Some(*lhs),
            _ => None,
        }
    }

    fn try_selected_index_in_place(
        &mut self,
        scope: &CheckerScope<'_>,
        statement: StmtId,
        array: ExprId,
        array_ty: Ty,
        indices: &[ExprId],
        value: ExprId,
    ) -> bool {
        let Expr::Binary { op, lhs, rhs, .. } = self.file.expr(value).clone() else {
            return false;
        };
        if self.compound_index_lhs(array, indices, value) != Some(lhs) {
            return false;
        }
        let element_ty = self.type_selected_index(scope, lhs, array_ty);
        if element_ty == Ty::Error {
            return false;
        }
        self.try_in_place_assignment_operands(scope, statement, op, lhs, rhs, Some(element_ty))
    }

    fn type_assignment_value_on_selected_index(
        &mut self,
        scope: &CheckerScope<'_>,
        array: ExprId,
        array_ty: Ty,
        indices: &[ExprId],
        value: ExprId,
        builtin_element: Option<Ty>,
    ) -> Ty {
        let expected = match builtin_element {
            Some(expected) => Some(expected),
            None => {
                let mut arguments = indices.to_vec();
                arguments.push(value);
                self.selected_operator_params(scope, array_ty, "set", &arguments)
                    .and_then(|params| params.last().copied())
            }
        };
        if let Some(lhs) = self.compound_index_lhs(array, indices, value) {
            let element_ty = self.type_selected_index(scope, lhs, array_ty);
            let expected = match expected {
                Some(expected) if builtin_element.is_none() => {
                    Some(self.declared_function_semantic_type(expected))
                }
                other => other,
            };
            return self.type_compound_on_prepared_lhs(scope, value, element_ty, expected);
        }
        match expected {
            Some(expected) if builtin_element.is_some() => {
                self.expr_expected(scope, value, expected)
            }
            Some(expected) => self.check_argument_expected(scope, value, expected, false, None),
            None => self.expr(scope, value),
        }
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
        let element_ty = self.type_selected_index(&selector_scope, element, member_ty);
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
        let element_ty = self.type_selected_index(&selector_scope, element, member_ty);
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
        let Expr::Binary { op, rhs, .. } = self.file.expr(value).clone() else {
            return false;
        };
        let Some((member, member_ty)) =
            self.safe_member_operand(&selector_scope, receiver, receiver_ty, value)
        else {
            return self.try_in_place_assignment(&selector_scope, statement, value);
        };
        if member_ty == Ty::Error {
            return false;
        }
        self.try_in_place_assignment_operands(
            &selector_scope,
            statement,
            op,
            member,
            rhs,
            Some(member_ty),
        )
    }

    /// Type the value of `receiver?.name = value`, including a compound `name op rhs`.
    ///
    /// The read inside a compound update is a plain member of the same receiver. Checked in the
    /// outer scope, that receiver is still nullable and the update is rejected. The selector
    /// scope has already proved it non-null. The member type selected there is the binary's left
    /// operand, so the read is not resolved against the nullable receiver again.
    pub(super) fn safe_member_assignment_value(
        &mut self,
        scope: &CheckerScope<'_>,
        safe: bool,
        receiver: ExprId,
        receiver_ty: Ty,
        value: ExprId,
        expected: Option<Ty>,
    ) -> Ty {
        let typecheck = |checker: &mut Self, scope: &CheckerScope<'_>| match expected {
            Some(expected) => checker.expr_expected(scope, value, expected),
            None => checker.expr(scope, value),
        };
        if !safe {
            return typecheck(self, scope);
        }
        let selector_scope = self.open_safe_selector_scope(scope, receiver, receiver_ty);
        if let Some((_, member_ty)) =
            self.safe_member_operand(&selector_scope, receiver, receiver_ty, value)
        {
            return self.type_compound_on_prepared_lhs(&selector_scope, value, member_ty, expected);
        }
        typecheck(self, &selector_scope)
    }

    /// The non-null member read inside `receiver?.name op= rhs`, when `value` is that compound.
    fn safe_member_operand(
        &mut self,
        scope: &CheckerScope<'_>,
        receiver: ExprId,
        receiver_ty: Ty,
        value: ExprId,
    ) -> Option<(ExprId, Ty)> {
        let Expr::Binary { lhs, .. } = self.file.expr(value).clone() else {
            return None;
        };
        let Expr::Member {
            receiver: read_receiver,
            name,
        } = self.file.expr(lhs).clone()
        else {
            return None;
        };
        if read_receiver != receiver {
            return None;
        }
        let safe_receiver = receiver_ty.non_null().definitely_non_null();
        let member_ty = self.check_member(scope, safe_receiver, &name, self.span(lhs), Some(lhs));
        self.set(lhs, member_ty);
        Some((lhs, member_ty))
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
        self.stmt_assign_index(
            &selector_scope,
            statement,
            access,
            indices,
            value,
            Some(member_ty),
        );
    }
    pub(super) fn stmt_assign_index(
        &mut self,
        scope: &CheckerScope<'_>,
        s: StmtId,
        array: ExprId,
        indices: Vec<ExprId>,
        value: ExprId,
        selected_receiver: Option<Ty>,
    ) {
        // Try `opAssign` on the value returned by `get` before requiring `set`.
        // A safe index already selected the member, so the element read must use that type
        // instead of typing the member again through the nullable receiver.
        if let Some(array_ty) = selected_receiver {
            if self.try_selected_index_in_place(scope, s, array, array_ty, &indices, value) {
                return;
            }
        } else if self.try_in_place_assignment(scope, s, value) {
            return;
        }
        // `a[i] = v` stores an array element; `recv[i, j, …] = v` calls `set` (or Map `put`).
        let at = match selected_receiver {
            Some(ty) => self.set(array, ty),
            None => self.expr(scope, array),
        };
        let its: Vec<Ty> = indices.iter().map(|&i| self.expr(scope, i)).collect();
        // Array access is the built-in member rung only when its `Int` index is applicable. An
        // inapplicable built-in does not hide a user operator extension such as
        // `operator fun IntArray.set(Long, Int)`.
        let builtin_array_element = matches!(indices.as_slice(), [_])
            .then(|| at.array_elem())
            .flatten()
            .filter(|_| self.receiver_is_assignable(its[0], Ty::Int));
        let vt = if selected_receiver.is_some() {
            self.type_assignment_value_on_selected_index(
                scope,
                array,
                at,
                &indices,
                value,
                builtin_array_element,
            )
        } else {
            match builtin_array_element {
                Some(expected) => self.expr_expected(scope, value, expected),
                None => {
                    let mut arguments = indices.clone();
                    arguments.push(value);
                    match self
                        .selected_operator_params(scope, at, "set", &arguments)
                        .and_then(|params| params.last().copied())
                    {
                        Some(expected) => {
                            self.check_argument_expected(scope, value, expected, false, None)
                        }
                        None => self.expr(scope, value),
                    }
                }
            }
        };
        let span = self.file.stmt_spans[s.0 as usize];
        let single_index = matches!(indices.as_slice(), [_]);
        if single_index {
            if let Some(elem) = builtin_array_element {
                self.expect_assignable(Ty::Int, its[0], span, "array index");
                self.expect_assignable(elem, vt, span, "array element assignment");
                return;
            }
        }
        if at == Ty::Error {
            return;
        }
        let mut set_args = its.clone();
        set_args.push(vt);
        let mut set_exprs = indices.clone();
        set_exprs.push(value);
        // Resolve `set` as a member, same-module extension, or library member. A single-index Map
        // store may resolve to `put`. Record the selected target so lowering does not choose again.
        let set_selected = self
            .operator_call_ret(scope, array, at, "set", &set_args, &set_exprs, span, None)
            .map(|(_, call)| (SyntheticOperatorCall::Set, call));
        if self.indexed_operator_ambiguous {
            return;
        }
        let selected = set_selected.or_else(|| {
            single_index
                .then(|| {
                    self.operator_call_ret(
                        scope, array, at, "put", &set_args, &set_exprs, span, None,
                    )
                })
                .flatten()
                .map(|(_, call)| (SyntheticOperatorCall::Put, call))
        });
        let ok = if let Some((op, call)) = selected {
            if single_index && matches!(&call, ResolvedCall::Member(_)) {
                if let Some(get) = self.select_instance_member(at, "get", &[its[0]]) {
                    self.resolved_index_store_get_returns.insert(s, get.ret);
                }
            }
            let selected_shape = match &call {
                ResolvedCall::Member(member) => Some((
                    member.member.params.as_slice(),
                    member.member.call_sig.vararg_index,
                )),
                ResolvedCall::Extension(extension) => {
                    Some((extension.params.as_slice(), extension.vararg_index))
                }
                _ => None,
            };
            if let Some((params, vararg)) = selected_shape {
                if let Some(slots) = indexed_operator_argument_slots(
                    params,
                    vararg,
                    &set_exprs,
                    op == SyntheticOperatorCall::Set,
                ) {
                    self.resolved_stmt_operator_arg_slots.insert((s, op), slots);
                }
            }
            self.resolved_stmt_operator_calls.insert((s, op), call);
            true
        } else {
            false
        };
        if !ok && at != Ty::Error {
            self.diags.error(
                span,
                if single_index {
                    format!(
                        "'{}' is not an array (cannot index-assign)",
                        at.source_name()
                    )
                } else {
                    format!(
                        "no 'set' operator taking {} indices on '{}'",
                        its.len(),
                        at.source_name()
                    )
                },
            );
        }
    }
}
