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

    /// Whether `x in a..b` has a REFERENCE value over a primitive range whose elements can actually
    /// inhabit it (`x: Any` over `4..10`). The membership test then reduces to "is `x` a boxed element
    /// of the range", so a value type unrelated to the boxed element (`x: String` over `4..10`) is
    /// excluded — that comparison is never non-trivially true and kotlinc rejects it.
    ///
    /// Only `Int`/`Long`/`Char` elements qualify, because the widened form is `Iterable<T>.contains`:
    /// a FLOATING-POINT range is a `ClosedFloatingPointRange`, not an `Iterable`, so kotlinc rejects
    /// `x: Any in 1.0..2.0` outright; a `Byte`/`Short` range is really an `IntRange` (its elements box
    /// to `Integer`, not to the bound's own wrapper); and an unsigned range's elements box to their
    /// inline class, which krusty erases to the signed primitive.
    fn in_range_widened_value(&self, value: Ty, element: Ty) -> bool {
        if !matches!(element, Ty::Int | Ty::Long | Ty::Char) {
            return false;
        }
        let Some(boxed) = element.boxed_ref() else {
            return false;
        };
        value.is_reference() && self.when_objs_comparable(value.non_null(), boxed)
    }

    pub(super) fn resolve_in_range_expression(
        &mut self,
        scope: &CheckerScope<'_>,
        e: ExprId,
        value: ExprId,
        start: ExprId,
        end: ExprId,
        kind: crate::ast::RangeKind,
    ) -> Ty {
        self.resolved_in_range_comparisons.remove(&e);
        let t = {
            let vt = self.expr(scope, value);
            let st = self.expr(scope, start);
            let et = self.expr(scope, end);
            // Built-in range overload selection uses a type parameter's declared upper bound. The
            // original symbolic type remains attached to each expression; these three values are
            // only the classifiers participating in `rangeTo`/`contains` selection.
            let prim = |t: &Ty| {
                matches!(
                    *t,
                    Ty::Int
                        | Ty::Long
                        | Ty::Char
                        | Ty::Short
                        | Ty::Byte
                        | Ty::Double
                        | Ty::Float
                        | Ty::UInt
                        | Ty::ULong
                )
            };
            let range_operand = |ty: Ty| {
                let primary = ty.range_operand_bound();
                if prim(&primary) {
                    primary
                } else {
                    self.semantic_tparam_extra_bounds(scope, ty)
                        .into_iter()
                        .map(Ty::range_operand_bound)
                        .find(prim)
                        .unwrap_or(primary)
                }
            };
            let range_vt = range_operand(vt);
            let range_st = range_operand(st);
            let range_et = range_operand(et);
            // Require uniform operand types — the lowering emits direct same-type comparisons, so a
            // mixed range (Int value, Long bounds) would need promotion that isn't modeled yet.
            if prim(&range_vt) && range_vt == range_st && range_st == range_et {
                // `Double`/`Float` `in a..b` is a comparison only for the stdlib floating range.
                // A nearer `operator fun Double.rangeTo` is an ordinary `rangeTo` + `contains`.
                // Integral ranges stay comparisons: their `rangeTo` members are range constructions,
                // and probing them would re-check every counted membership.
                if matches!(st, Ty::Double | Ty::Float)
                    && st == et
                    && st == vt
                    && kind == crate::ast::RangeKind::Through
                {
                    let operands = InRangeOperands {
                        expression: e,
                        end,
                        value,
                        start_ty: st,
                        end_ty: et,
                        value_ty: vt,
                    };
                    if let Some(resolved) =
                        self.shadowed_floating_range_membership(scope, &operands)
                    {
                        return self.set(e, resolved);
                    }
                }
                let comparison = range_st.range_counter_type().unwrap_or(range_st);
                self.resolved_in_range_comparisons.insert(e, comparison);
                Ty::Boolean
            } else if prim(&range_st)
                && range_st == range_et
                && self.in_range_widened_value(vt, range_st)
            {
                // A WIDENED value over a primitive range: `when (x: Any) { in 4..10 -> … }`. kotlinc
                // lowers it to `CollectionsKt.contains(4..10, x)`, which is true exactly when `x` is a
                // BOXED element of the range — so it stays a comparison chain, guarded by the
                // `instanceof` the boxed element type implies.
                let comparison = range_st
                    .range_counter_type()
                    .expect("widened direct membership is restricted to counted primitive ranges");
                self.resolved_in_range_comparisons.insert(e, comparison);
                Ty::Boolean
            } else {
                // Every non-direct-comparison range desugars through the ordinary declarations
                // `a.rangeTo(b).contains(x)`. This includes reference operators AND mixed unsigned
                // membership (`UByte in UIntRange`, `UInt in ULongRange`), whose `contains` overloads
                // live in stdlib metadata. Scalar storage is irrelevant to source applicability.
                let operands = InRangeOperands {
                    expression: e,
                    end,
                    value,
                    start_ty: st,
                    end_ty: et,
                    value_ty: vt,
                };
                if let Some(resolved) = self.record_range_contains(scope, &operands) {
                    return self.set(e, resolved);
                }
                self.diags.error(
                    self.span(e),
                    format!(
                        "operator 'contains' cannot be applied to range '{}' and '{}'",
                        st.source_name(),
                        vt.source_name()
                    ),
                );
                Ty::Error
            }
        };
        self.set(e, t)
    }

    /// `None` when `rangeTo` is the stdlib floating membership (or is absent): the caller keeps the
    /// comparison. `Some` when a nearer operator was selected, including a failed `contains`.
    fn shadowed_floating_range_membership(
        &mut self,
        scope: &CheckerScope<'_>,
        operands: &InRangeOperands,
    ) -> Option<Ty> {
        let (range_ty, range_call) = self.operator_call_ret(
            scope,
            operands.expression,
            operands.start_ty,
            "rangeTo",
            &[operands.end_ty],
            &[operands.end],
            self.span(operands.expression),
            None,
        )?;
        if matches!(
            &range_call,
            ResolvedCall::Extension(extension)
                if extension.callable.compiler_intrinsic
                    == Some(crate::libraries::CompilerIntrinsic::FloatingRangeMembership)
        ) {
            return None;
        }
        Some(self.finish_range_contains(scope, operands, range_ty, range_call))
    }

    /// Record `rangeTo` + `contains` when both resolve. `None` leaves the caller's diagnostic in place.
    fn record_range_contains(
        &mut self,
        scope: &CheckerScope<'_>,
        operands: &InRangeOperands,
    ) -> Option<Ty> {
        let (range_ty, range_call) = self.operator_call_ret(
            scope,
            operands.expression,
            operands.start_ty,
            "rangeTo",
            &[operands.end_ty],
            &[operands.end],
            self.span(operands.expression),
            None,
        )?;
        Some(self.finish_range_contains(scope, operands, range_ty, range_call))
    }

    fn finish_range_contains(
        &mut self,
        scope: &CheckerScope<'_>,
        operands: &InRangeOperands,
        range_ty: Ty,
        range_call: ResolvedCall,
    ) -> Ty {
        let Some((Ty::Boolean, contains_call)) = self.operator_call_ret(
            scope,
            operands.expression,
            range_ty,
            "contains",
            &[operands.value_ty],
            &[operands.value],
            self.span(operands.expression),
            None,
        ) else {
            self.diags.error(
                self.span(operands.expression),
                format!(
                    "operator 'contains' cannot be applied to range '{}' and '{}'",
                    operands.start_ty.source_name(),
                    operands.value_ty.source_name()
                ),
            );
            return Ty::Error;
        };
        self.resolved_operator_calls.insert(
            (operands.expression, SyntheticOperatorCall::RangeTo),
            range_call,
        );
        self.resolved_operator_calls.insert(
            (operands.expression, SyntheticOperatorCall::Contains),
            contains_call,
        );
        Ty::Boolean
    }
}

struct InRangeOperands {
    expression: ExprId,
    end: ExprId,
    value: ExprId,
    start_ty: Ty,
    end_ty: Ty,
    value_ty: Ty,
}
