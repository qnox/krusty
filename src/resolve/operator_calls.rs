//! Selection and checking of Kotlin's operator conventions.
//!
//! An operator call is an ordinary callable selection wearing source syntax: `a + b` picks `plus`,
//! `a[i] = v` picks `set`, `a in b` picks `contains`. This module owns that translation and the
//! argument checking that follows it, including the one input the syntax hides — the call's EXPECTED
//! RESULT, which a formal shared between an operator's result and its operands can only be settled
//! by (`Iterable<T>.plus(Iterable<T>): List<T>` against a declared `List<P>`).
//!
//! Extracted from the resolver root: the root is migration debt that may not grow, and this is a
//! cohesive responsibility with a narrow boundary — it consumes a receiver, a convention name and
//! the argument expressions, and returns the selected call's result type plus its resolved target.

use super::*;

impl<'a> Checker<'a> {
    pub(super) fn operator_call_ret(
        &mut self,
        scope: &CheckerScope<'_>,
        site: ExprId,
        receiver: Ty,
        name: &str,
        arg_tys: &[Ty],
        arg_exprs: &[ExprId],
        span: Span,
        expected_result: Option<Ty>,
    ) -> Option<(Ty, ResolvedCall)> {
        // `downTo` and the legacy `until` range spellings are represented by RangeKind in the AST,
        // but their Kotlin declarations are `infix`, not `operator`. They still go through the same
        // semantic callable selection and checked-FIR handoff as the operator range conventions.
        let accepts_range_infix = matches!(name, "downTo" | "until");
        let has_convention_modifier =
            |operator: bool, infix: bool| operator || (accepts_range_infix && infix);
        if name == "set" {
            self.indexed_operator_ambiguous = false;
            let arg_kinds = self.call_arg_kinds(scope, arg_exprs);
            let (mut functions, properties) =
                self.stable_receiver_callables(receiver, name).into_parts();
            let resolver = self.resolver();
            functions.overloads.retain(|candidate| {
                candidate.flags.operator
                    && (candidate.kind == crate::libraries::FnKind::Member
                        || self.source_callable_visible(candidate))
                    && (candidate.kind != crate::libraries::FnKind::Member
                        || candidate.context_count == 0)
            });
            let callables = crate::libraries::Callables::from_parts(functions, properties);
            let indexed = resolver.select_receiver_indexed_set_function_with_params(
                receiver,
                name,
                &arg_kinds,
                &[],
                &callables,
            );
            drop(resolver);
            let (selected, params, ret) = match indexed {
                crate::symbol_resolver::CandidateSelection::Selected(selected) => selected,
                crate::symbol_resolver::CandidateSelection::Ambiguous => {
                    self.indexed_operator_ambiguous = true;
                    self.diags.error(
                        span,
                        "overload resolution ambiguity for operator 'set'".to_string(),
                    );
                    return None;
                }
                crate::symbol_resolver::CandidateSelection::None => {
                    return match self.member_extension_operator_call(
                        scope, site, receiver, name, arg_exprs, arg_tys, span,
                    ) {
                        Ok(selected) => selected,
                        Err(()) => {
                            self.indexed_operator_ambiguous = true;
                            self.diags.error(
                                span,
                                "overload resolution ambiguity for operator 'set'".to_string(),
                            );
                            None
                        }
                    };
                }
            };
            let vararg = selected
                .call_sig
                .vararg_index
                .and_then(|index| index.checked_sub(selected.context_count));
            if !self
                .expect_indexed_operator_params(scope, &params, vararg, true, arg_exprs, arg_tys)
            {
                return None;
            }
            let semantic = selected.semantic_signature();
            let context_count = selected.context_count.min(semantic.params.len());
            let context_args = if context_count == 0 {
                Vec::new()
            } else if let Some(context) =
                self.select_context_arguments(scope, &semantic.params[..context_count])
            {
                context
            } else {
                self.diags.error(
                    span,
                    "no implicit value is available for the context parameters".to_string(),
                );
                return None;
            };
            if selected.kind == crate::libraries::FnKind::Member
                && !self.member_accessible(selected.visibility, selected.callable.owner)
            {
                self.reject_if_inaccessible(
                    selected.visibility,
                    name,
                    selected.callable.owner,
                    span,
                );
                return None;
            }
            let target = if selected.kind == crate::libraries::FnKind::Member {
                let mut member = selected.member_with_return(ret);
                member.params = params;
                ResolvedCall::Member(crate::symbol_resolver::ResolvedMember {
                    receiver,
                    physical_params: selected.callable.physical_params.clone(),
                    context_args: context_args.into_iter().map(Some).collect(),
                    ret,
                    member,
                    projected_return_hazard: selected.projected_return_hazard,
                    suspend: selected.flags.suspend,
                    origin: selected.callable.origin.clone(),
                })
            } else {
                let mut callable = selected.callable.clone();
                callable.ret = ret;
                ResolvedCall::source_extension(
                    callable,
                    receiver,
                    params,
                    context_args,
                    ret,
                    selected.source_key,
                    selected.stable_declaration,
                    vararg.is_some(),
                    vararg,
                    selected
                        .default_values
                        .get(selected.context_count..)
                        .unwrap_or_default()
                        .to_vec(),
                )
            };
            return Some((ret, target));
        }
        let arg_kinds = self.call_arg_kinds(scope, arg_exprs);
        crate::trace_compiler!(
            "resolve",
            "operator extension inventory name={name} receiver={receiver:?} candidates={:?}",
            self.stable_receiver_callables(receiver, name)
                .functions()
                .iter()
                .map(|candidate| (
                    candidate.flags.operator,
                    candidate.semantic_receiver(),
                    candidate.semantic_params().to_vec(),
                    candidate.applied_params().to_vec(),
                ))
                .collect::<Vec<_>>()
        );
        let (mut functions, properties) =
            self.stable_receiver_callables(receiver, name).into_parts();
        functions.overloads.retain(|candidate| {
            has_convention_modifier(candidate.flags.operator, candidate.flags.infix)
                && (candidate.kind == crate::libraries::FnKind::Member
                    || self.source_callable_visible(candidate))
                && (candidate.kind != crate::libraries::FnKind::Member
                    || candidate.context_count == 0)
        });
        let mut members = functions.clone();
        members
            .overloads
            .retain(|candidate| candidate.kind == crate::libraries::FnKind::Member);
        let members = crate::libraries::Callables::Functions(members);
        let callables = crate::libraries::Callables::from_parts(functions, properties);
        let member_applicable = {
            !matches!(
                self.resolver()
                    .select_receiver_function_with_params_tracking(
                        receiver,
                        name,
                        &arg_kinds,
                        &[],
                        &members,
                        None,
                    ),
                crate::symbol_resolver::CandidateSelection::None
            )
        };
        if !member_applicable {
            match self.select_local_extension_candidate(
                scope, receiver, name, arg_exprs, arg_tys, None, false,
            ) {
                LocalExtensionSelection::Selected(selected)
                    if has_convention_modifier(
                        selected.signature.is_operator(),
                        selected.signature.is_infix(),
                    ) =>
                {
                    let context_count = selected
                        .signature
                        .context_count
                        .min(selected.signature.params.len());
                    self.expect_call_args(
                        scope,
                        &selected.signature.params[context_count..],
                        selected.signature.vararg(),
                        arg_exprs,
                        arg_tys,
                    );
                    let ret = selected.signature.ret;
                    return Some((
                        ret,
                        ResolvedCall::LocalFunction(Box::new(ResolvedLocalFunctionCall {
                            stmt_id: selected.statement,
                            sig: selected.signature,
                            provided_arg_count: arg_exprs.len(),
                            context_args: selected.context_args,
                            receiver: None,
                        })),
                    ));
                }
                LocalExtensionSelection::Selected(_)
                | LocalExtensionSelection::Ambiguous
                | LocalExtensionSelection::None => {}
            }
            match self.member_extension_operator_call(
                scope, site, receiver, name, arg_exprs, arg_tys, span,
            ) {
                Ok(Some(selected)) => return Some(selected),
                Ok(None) => {}
                Err(()) => {
                    self.diags.error(
                        span,
                        format!("overload resolution ambiguity for member '{name}'"),
                    );
                    return None;
                }
            }
        }
        let resolver = self.resolver();
        // The expected result joins the receiver and the arguments as an input to the ONE
        // instantiation that yields the parameter vector, the result type and the recorded target,
        // so they cannot disagree about a formal the result and the operands share.
        let (selected, params, ret) = match resolver.select_receiver_function_with_params_tracking(
            receiver,
            name,
            &arg_kinds,
            &[],
            &callables,
            expected_result,
        ) {
            crate::symbol_resolver::CandidateSelection::Selected(selected) => selected,
            crate::symbol_resolver::CandidateSelection::None
            | crate::symbol_resolver::CandidateSelection::Ambiguous => return None,
        };
        let semantic = selected.semantic_signature();
        let context_count = selected.context_count.min(semantic.params.len());
        let context_args = if context_count == 0 {
            Vec::new()
        } else if let Some(context) =
            self.select_context_arguments(scope, &semantic.params[..context_count])
        {
            context
        } else {
            self.diags.error(
                span,
                "no implicit value is available for the context parameters".to_string(),
            );
            return None;
        };
        if selected.kind == crate::libraries::FnKind::Member
            && !self.member_accessible(selected.visibility, selected.callable.owner)
        {
            self.reject_if_inaccessible(selected.visibility, name, selected.callable.owner, span);
            return None;
        }
        self.expect_call_args(scope, &params, false, arg_exprs, arg_tys);
        let target = if selected.kind == crate::libraries::FnKind::Member {
            let mut member = selected.member_with_return(ret);
            member.params = params;
            ResolvedCall::Member(crate::symbol_resolver::ResolvedMember {
                receiver,
                physical_params: selected.callable.physical_params.clone(),
                context_args: Vec::new(),
                ret,
                member,
                projected_return_hazard: selected.projected_return_hazard,
                suspend: selected.flags.suspend,
                origin: selected.callable.origin.clone(),
            })
        } else {
            let vararg = selected
                .call_sig
                .vararg_index
                .and_then(|index| index.checked_sub(selected.context_count));
            let mut callable = selected.callable.clone();
            callable.ret = ret;
            ResolvedCall::source_extension(
                callable,
                receiver,
                params,
                context_args,
                ret,
                selected.source_key,
                selected.stable_declaration,
                vararg.is_some(),
                vararg,
                selected
                    .default_values
                    .get(selected.context_count..)
                    .unwrap_or_default()
                    .to_vec(),
            )
        };
        Some((ret, target))
    }

    /// Parameter types of the one applicable operator overload, before any postponed argument body is
    /// checked. Members and in-scope extensions are selected from the same federated callable set.
    pub(super) fn selected_operator_params(
        &mut self,
        scope: &CheckerScope<'_>,
        receiver: Ty,
        name: &str,
        args: &[ExprId],
    ) -> Option<Vec<Ty>> {
        let argument_kinds = self.call_arg_kinds(scope, args);
        let (mut functions, _) = self.stable_receiver_callables(receiver, name).into_parts();
        let resolver = self.resolver();
        functions.overloads.retain(|candidate| {
            candidate.flags.operator
                && self.source_callable_visible(candidate)
                && (candidate.kind != crate::libraries::FnKind::Member
                    || candidate.context_count == 0)
        });
        let callables = crate::libraries::Callables::Functions(functions);
        if name == "set" {
            match resolver.select_receiver_indexed_set_function_with_params(
                receiver,
                name,
                &argument_kinds,
                &[],
                &callables,
            ) {
                crate::symbol_resolver::CandidateSelection::Selected((_, params, _)) => {
                    Some(params)
                }
                crate::symbol_resolver::CandidateSelection::None
                | crate::symbol_resolver::CandidateSelection::Ambiguous => None,
            }
        } else {
            resolver
                .select_receiver_function_with_params(
                    receiver,
                    name,
                    &argument_kinds,
                    &[],
                    &callables,
                )
                .map(|(_, params)| params)
        }
    }
}
