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

/// Exact classifier roles selected for a builtin operator that has no retained [`ResolvedCall`].
/// Builtin scalar arithmetic and increment/decrement lower directly, but their semantic access still
/// belongs to checking; diagnostic consumers must not reconstruct it from syntax or result types.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct SelectedBuiltinOperatorAccess {
    pub(super) dispatch_receiver: Option<TypeName>,
    pub(super) owner: Option<TypeName>,
    pub(super) extension_receiver: Option<TypeName>,
}

impl SelectedBuiltinOperatorAccess {
    pub(super) fn member(receiver: Ty) -> Option<Self> {
        let receiver = receiver.non_null().kotlin_class_internal();
        Some(Self {
            dispatch_receiver: receiver,
            owner: receiver,
            extension_receiver: None,
        })
        .filter(|access| access.dispatch_receiver.is_some())
    }

    pub(super) fn selected(call: &ResolvedCall) -> Option<Self> {
        let class_of = |ty: Ty| ty.non_null().kotlin_class_internal();
        Some(match call {
            ResolvedCall::Member(selected) => Self {
                dispatch_receiver: class_of(selected.receiver),
                owner: selected.member.owner,
                extension_receiver: None,
            },
            ResolvedCall::Companion(member) => Self {
                dispatch_receiver: None,
                owner: member.owner,
                extension_receiver: None,
            },
            ResolvedCall::Extension(call) => Self {
                dispatch_receiver: None,
                owner: None,
                extension_receiver: call.callable.source_receiver.and_then(class_of),
            },
            ResolvedCall::MemberExtension {
                owner,
                extension_receiver,
                ..
            } => Self {
                dispatch_receiver: None,
                owner: Some(*owner),
                extension_receiver: class_of(*extension_receiver),
            },
            ResolvedCall::TopLevel(_) | ResolvedCall::LocalFunction(_) => return None,
        })
    }
}

/// The synthetic-call key for a range syntax, paired with the declaration name `RangeKind` owns.
/// Membership, range expressions, and loops share the key so a mixed operand such as
/// `3f in 1.0..<3.0` is not rewritten through the wrong operator. Membership always selects this
/// declaration before its exact provider realization may authorize a direct comparison.
#[derive(Clone, Copy)]
pub(super) struct RangeConvention {
    pub(super) key: SyntheticOperatorCall,
    pub(super) name: &'static str,
}

pub(super) fn range_operator(kind: crate::ast::RangeKind) -> RangeConvention {
    let name = kind.operator_name();
    let key =
        SyntheticOperatorCall::from_name(name).expect("range syntax names a synthetic operator");
    RangeConvention { key, name }
}

/// One selected operator convention: its result, its resolved target, and what its selection
/// solved for the declaration's own type parameters.
struct SelectedOperator {
    ret: Ty,
    call: ResolvedCall,
    type_arguments: OperatorTypeArguments,
}

/// The declaration's generic signature, the selected call shape, and one solved argument per
/// declared formal (`None` when the selection left that formal unbound).
struct SolvedOperatorTypeArguments {
    signature: crate::libraries::GenericSig,
    call_sig: CallSig,
    arguments: Vec<Option<Ty>>,
}

/// The type arguments an operator selection solved, published only by the caller whose expression
/// IS that one call (`a + b`, `a..b`, `a < b`). A convention selected for another expression's
/// key (a loop range, a compound assignment receiver, an `in` range that also selects `contains`)
/// does not own that key's single generic-call record.
enum OperatorTypeArguments {
    Solved(Box<SolvedOperatorTypeArguments>),
    NotGeneric,
    /// The selection path committed its own generic facts (member extensions) or has none to
    /// commit here (local extensions and indexed `set`, which their own paths record).
    CommittedBySelection,
}

/// The single right-hand operand of a binary operator expression (`b` in `a + b`, `a..b`,
/// `a < b`): its checked type and its source expression.
pub(super) struct OperatorOperand {
    pub(super) ty: Ty,
    pub(super) expression: ExprId,
}

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
        self.select_operator(
            scope,
            site,
            receiver,
            name,
            arg_tys,
            arg_exprs,
            span,
            expected_result,
            None,
        )
        .map(|selected| (selected.ret, selected.call))
    }

    /// Select the convention `name` that the binary expression `expression` denotes (`a + b`,
    /// `a..b`) and record its target under the expression's synthetic operator call. Returns the
    /// convention's result.
    pub(super) fn select_binary_operator_expression(
        &mut self,
        scope: &CheckerScope<'_>,
        expression: ExprId,
        receiver: Ty,
        name: &str,
        operand: OperatorOperand,
        expected_result: Option<Ty>,
    ) -> Option<Ty> {
        let key = SyntheticOperatorCall::from_name(name)
            .expect("a binary operator expression names a synthetic operator");
        let (ret, call) = self.operator_expression_call_ret(
            scope,
            expression,
            receiver,
            name,
            operand,
            expected_result,
        )?;
        self.resolved_operator_calls.insert((expression, key), call);
        Some(ret)
    }

    /// Select the `compareTo` convention a comparison expression (`a < b`) denotes. Only a
    /// convention returning `Int` is a comparison; its target is then recorded under the
    /// expression's synthetic `compareTo` call.
    pub(super) fn select_comparison_expression(
        &mut self,
        scope: &CheckerScope<'_>,
        expression: ExprId,
        receiver: Ty,
        operand: OperatorOperand,
    ) -> bool {
        match self.operator_expression_call_ret(
            scope,
            expression,
            receiver,
            "compareTo",
            operand,
            None,
        ) {
            Some((Ty::Int, call)) => {
                self.resolved_operator_calls
                    .insert((expression, SyntheticOperatorCall::CompareTo), call);
                true
            }
            Some(_) | None => false,
        }
    }

    /// Select the operator convention that `expression` itself denotes and publish the type
    /// arguments it solved under that expression, as every other generic call publishes them:
    /// `a + b` through `inline operator fun <reified T> T.plus(other: T)` passes the same reified
    /// `T` as the explicit `a.plus(b)`.
    fn operator_expression_call_ret(
        &mut self,
        scope: &CheckerScope<'_>,
        expression: ExprId,
        receiver: Ty,
        name: &str,
        operand: OperatorOperand,
        expected_result: Option<Ty>,
    ) -> Option<(Ty, ResolvedCall)> {
        let selected = self.select_operator(
            scope,
            expression,
            receiver,
            name,
            &[operand.ty],
            &[operand.expression],
            self.span(expression),
            expected_result,
            None,
        )?;
        match selected.type_arguments {
            OperatorTypeArguments::Solved(solved) => {
                let SolvedOperatorTypeArguments {
                    signature,
                    call_sig,
                    arguments,
                } = *solved;
                self.replace_selected_generic_call(expression, &signature, &call_sig, arguments)
            }
            OperatorTypeArguments::NotGeneric => self.clear_selected_generic_call(expression),
            OperatorTypeArguments::CommittedBySelection => {}
        }
        Some((selected.ret, selected.call))
    }

    /// Shared operator selection with an optional checked semantic view of the arguments. Range
    /// membership uses this only to authorize a direct comparison through a type parameter's
    /// declared primitive bound; the ordinary call path always derives kinds from source values.
    fn select_operator(
        &mut self,
        scope: &CheckerScope<'_>,
        site: ExprId,
        receiver: Ty,
        name: &str,
        arg_tys: &[Ty],
        arg_exprs: &[ExprId],
        span: Span,
        expected_result: Option<Ty>,
        checked_argument_kinds: Option<Vec<crate::symbol_resolver::CallArgKind>>,
    ) -> Option<SelectedOperator> {
        // `downTo` and the legacy `until` range spellings are represented by RangeKind in the AST,
        // but their Kotlin declarations are `infix`, not `operator`. They still go through the same
        // semantic callable selection and checked-FIR handoff as the operator range conventions.
        let accepts_range_infix = matches!(name, "downTo" | "until");
        let has_convention_modifier =
            |operator: bool, infix: bool| operator || (accepts_range_infix && infix);
        if name == "set" {
            self.indexed_operator_ambiguous = false;
            let arg_kinds = checked_argument_kinds
                .clone()
                .unwrap_or_else(|| self.call_arg_kinds(scope, arg_exprs));
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
                        scope,
                        CallArgs {
                            call: site,
                            args: arg_exprs,
                            arg_tys,
                        },
                        receiver,
                        name,
                        span,
                    ) {
                        Ok(selected) => selected.map(|(ret, call)| SelectedOperator {
                            ret,
                            call,
                            type_arguments: OperatorTypeArguments::CommittedBySelection,
                        }),
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
                && !self.callable_accessible(
                    selected.visibility,
                    selected.callable.owner,
                    selected.callable.external_identity,
                )
            {
                self.reject_callable_if_inaccessible(
                    selected.visibility,
                    name,
                    selected.callable.owner,
                    selected.callable.external_identity,
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
                ResolvedCall::source_extension(ResolvedSourceExtensionCall {
                    callable,
                    receiver,
                    params,
                    context_args: context_args.into_iter().map(Some).collect(),
                    ret,
                    source: selected.source_key,
                    stable_declaration: selected.stable_declaration,
                    vararg: vararg.is_some(),
                    vararg_index: vararg,
                    param_default_values: selected
                        .default_values
                        .get(selected.context_count..)
                        .unwrap_or_default()
                        .to_vec(),
                })
            };
            return Some(SelectedOperator {
                ret,
                call: target,
                type_arguments: OperatorTypeArguments::CommittedBySelection,
            });
        }
        let arg_kinds =
            checked_argument_kinds.unwrap_or_else(|| self.call_arg_kinds(scope, arg_exprs));
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
                scope,
                name,
                LocalExtensionSelectionRequest {
                    receiver,
                    args: arg_exprs,
                    arg_tys,
                    argument_names: None,
                    trailing_lambda: false,
                    supplied_type_arguments: None,
                },
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
                    return Some(SelectedOperator {
                        ret,
                        call: ResolvedCall::LocalFunction(Box::new(ResolvedLocalFunctionCall {
                            stmt_id: selected.statement,
                            sig: selected.signature,
                            provided_arg_count: arg_exprs.len(),
                            context_args: selected.context_args,
                            receiver: None,
                        })),
                        type_arguments: OperatorTypeArguments::CommittedBySelection,
                    });
                }
                LocalExtensionSelection::Selected(_)
                | LocalExtensionSelection::Ambiguous
                | LocalExtensionSelection::None => {}
            }
            match self.member_extension_operator_call(
                scope,
                CallArgs {
                    call: site,
                    args: arg_exprs,
                    arg_tys,
                },
                receiver,
                name,
                span,
            ) {
                Ok(Some((ret, call))) => {
                    return Some(SelectedOperator {
                        ret,
                        call,
                        type_arguments: OperatorTypeArguments::CommittedBySelection,
                    })
                }
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
        let crate::symbol_resolver::SelectedReceiverFunction {
            function: selected,
            params,
            ret,
            bindings,
            ..
        } = match resolver.select_receiver_function_with_applied_receiver_tracking(
            receiver,
            name,
            &arg_kinds,
            &[],
            &callables,
            expected_result,
        ) {
            crate::symbol_resolver::ReceiverFunctionSelection::Selected(selected) => selected,
            crate::symbol_resolver::ReceiverFunctionSelection::None
            | crate::symbol_resolver::ReceiverFunctionSelection::Ambiguous(_) => return None,
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
            && !self.callable_accessible(
                selected.visibility,
                selected.callable.owner,
                selected.callable.external_identity,
            )
        {
            self.reject_callable_if_inaccessible(
                selected.visibility,
                name,
                selected.callable.owner,
                selected.callable.external_identity,
                span,
            );
            return None;
        }
        self.expect_call_args(scope, &params, false, arg_exprs, arg_tys);
        let target = if selected.kind == crate::libraries::FnKind::Member {
            selected_member_call(receiver, &selected, params, ret)
        } else {
            let vararg = selected
                .call_sig
                .vararg_index
                .and_then(|index| index.checked_sub(selected.context_count));
            let mut callable = selected.callable.clone();
            callable.ret = ret;
            ResolvedCall::source_extension(ResolvedSourceExtensionCall {
                callable,
                receiver,
                params,
                context_args: context_args.into_iter().map(Some).collect(),
                ret,
                source: selected.source_key,
                stable_declaration: selected.stable_declaration,
                vararg: vararg.is_some(),
                vararg_index: vararg,
                param_default_values: selected
                    .default_values
                    .get(selected.context_count..)
                    .unwrap_or_default()
                    .to_vec(),
            })
        };
        let type_arguments = match selected.generic_sig.as_ref() {
            Some(signature) => {
                OperatorTypeArguments::Solved(Box::new(SolvedOperatorTypeArguments {
                    arguments: signature
                        .formals
                        .iter()
                        .map(|formal| bindings.get(formal).copied())
                        .collect(),
                    signature: signature.clone(),
                    call_sig: selected.call_sig.clone(),
                }))
            }
            None => OperatorTypeArguments::NotGeneric,
        };
        Some(SelectedOperator {
            ret,
            call: target,
            type_arguments,
        })
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
        let convention = range_operator(kind);
        for operator in [
            convention.key,
            SyntheticOperatorCall::Contains,
            SyntheticOperatorCall::CompareTo,
        ] {
            self.resolved_operator_calls.remove(&(e, operator));
            self.resolved_builtin_operator_accesses
                .remove(&(e, operator));
        }
        let t = {
            let vt = self.expr(scope, value);
            let st = self.expr(scope, start);
            let et = self.expr(scope, end);
            let operands = InRangeOperands {
                expression: e,
                kind,
                value_ty: vt,
            };
            let Some((range_ty, range_call)) = self.operator_call_ret(
                scope,
                e,
                st,
                convention.name,
                &[et],
                &[end],
                self.span(e),
                None,
            ) else {
                self.diags.error(
                    self.span(e),
                    format!(
                        "operator '{}' cannot be applied to '{}' and '{}'",
                        convention.name,
                        st.source_name(),
                        et.source_name()
                    ),
                );
                return self.set(e, Ty::Error);
            };
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
            let comparison_provenance = selected_range_comparison_provenance(kind, &range_call);
            let operands_are_numeric = [range_vt, range_st, range_et].iter().all(prim);
            let uniform = range_vt == range_st && range_st == range_et;
            let mut ordinary_contains = None;
            // kotlinc's `RangeContainsLowering` compares in the widest numeric type of the bounds
            // and the value (`computeComparisonClass`) when the call is the range's own `contains`
            // or a stdlib closed-range extension. Every primitive range declares the member
            // `contains` of its own element type, and a member wins over any extension, so operands
            // of one type always select it; mixed operands select through ordinary resolution.
            // Unsigned membership is compared only between operands of one type. A range kotlinc
            // does not compare (`downTo`, a floating `..<`) calls its `contains`.
            let mut unsigned_compare = None;
            let comparison =
                if operands_are_numeric {
                    // Resolve `contains` before bypassing it even when the source operand types are
                    // uniform. `Byte..Byte` and `Short..Short` build an `IntRange`, whose applicable
                    // narrow overload is an extension and may be shadowed by a local declaration.
                    // A type parameter participates through its declared primitive bound here,
                    // just as it does in `rangeTo` selection above. Keep the source type for the
                    // ordinary-call fallback below: only the selected builtin role authorizes
                    // bypassing that call.
                    let selected = if range_vt == vt {
                        self.range_contains_call(scope, e, range_ty, vt, value)
                    } else {
                        self.range_contains_call_for_bound(scope, e, range_ty, range_vt, value)
                    };
                    let selects_range_contains = match &selected {
                        Some((Ty::Boolean, ResolvedCall::Member(_))) => true,
                        Some((Ty::Boolean, ResolvedCall::Extension(extension))) => {
                            extension.callable.compiler_intrinsic
                                == Some(crate::libraries::CompilerIntrinsic::ClosedRangeContains)
                        }
                        _ => false,
                    };
                    if range_vt == vt {
                        ordinary_contains = Some(selected);
                    }
                    let unsigned = [range_vt, range_st, range_et]
                        .iter()
                        .any(|ty| ty.is_unsigned());
                    let comparison = (selects_range_contains
                        && comparison_provenance.is_some()
                        && (uniform || !unsigned))
                        .then(|| range_comparison_type(range_st, range_et, range_vt))
                        .flatten();
                    if let Some(counter @ (Ty::UInt | Ty::ULong)) = comparison {
                        unsigned_compare = self.unsigned_loop_compare(counter);
                        unsigned_compare.as_ref().map(|_| counter)
                    } else {
                        comparison
                    }
                } else if prim(&range_st)
                    && range_st == range_et
                    && self.in_range_widened_value(vt, range_st)
                {
                    // A WIDENED value over a primitive range: `when (x: Any) { in 4..10 -> … }`. kotlinc
                    // lowers it to `CollectionsKt.contains(4..10, x)`, which is true exactly when `x` is a
                    // BOXED element of the range — so it stays a comparison chain, guarded by the
                    // `instanceof` the boxed element type implies.
                    Some(range_st.range_counter_type().expect(
                        "widened direct membership is restricted to counted primitive ranges",
                    ))
                } else {
                    None
                };
            if let (Some(comparison), Some(provenance)) = (comparison, comparison_provenance) {
                if let Some(access) = SelectedBuiltinOperatorAccess::selected(&range_call) {
                    self.resolved_builtin_operator_accesses
                        .insert((e, convention.key), access);
                }
                // Direct comparison bypasses the selected range call, so publish the non-null
                // boundaries it would impose on both bounds. Use the already selected comparison
                // type: for a mixed floating range this preserves the platform check before the
                // same Float-to-Double widening the comparison requires.
                for operand in [start, end] {
                    self.narrow_platform_value(comparison, operand, PlatformNarrowing::Argument);
                }
                // Numeric membership similarly bypasses the selected contains call. The alternate
                // direct arm is a widened reference (`Any! in 1..2`): it must stay boxed for the
                // checked `instanceof Int` path and therefore has no scalar value boundary.
                if operands_are_numeric {
                    self.narrow_platform_value(comparison, value, PlatformNarrowing::Argument);
                }
                self.resolved_in_range_comparisons.insert(
                    e,
                    ResolvedInRangeComparison {
                        comparison,
                        provenance,
                        unsigned_compare,
                    },
                );
                Ty::Boolean
            } else {
                // Every non-direct-comparison range desugars through the ordinary declarations
                // `a.rangeTo(b).contains(x)`. This includes reference operators AND mixed unsigned
                // membership (`UByte in UIntRange`, `UInt in ULongRange`), whose `contains` overloads
                // live in stdlib metadata. Scalar storage is irrelevant to source applicability.
                let contains = ordinary_contains
                    .unwrap_or_else(|| self.range_contains_call(scope, e, range_ty, vt, value));
                let comparable_range = range_call.range_provider_realization()
                    == Some(crate::libraries::MemberRealization::Intrinsic(
                        crate::libraries::CompilerIntrinsic::ComparableRangeTo,
                    ))
                    && matches!(contains, Some((Ty::Boolean, ResolvedCall::Member(_))));
                let ty = self.finish_range_contains(&operands, range_ty, range_call, contains);
                if comparable_range && ty == Ty::Boolean {
                    self.select_comparable_range_ordering(e, range_ty);
                }
                ty
            }
        };
        self.set(e, t)
    }

    /// kotlinc's `RangeContainsLowering` over a `ComparableRangeToHandler` range: the range's own
    /// `contains` is replaced by `Comparable.compareTo` of the value with each bound. Select that
    /// member on `Comparable<T>` for the range's element `T`, through the ordinary member
    /// candidate selection, and record it beside the range and `contains` calls it stands in for.
    /// The bounds keep the boundaries `rangeTo` gave them, so the selection sees only the element
    /// type.
    fn select_comparable_range_ordering(&mut self, expression: ExprId, range_ty: Ty) {
        let Some(&element) = range_ty.type_args().first() else {
            return;
        };
        let comparable = Ty::obj_args_name(crate::types::wk::comparable(), &[element]);
        let (mut functions, _) = self
            .stable_receiver_callables(comparable, "compareTo")
            .into_parts();
        functions.overloads.retain(|candidate| {
            candidate.flags.operator
                && candidate.kind == crate::libraries::FnKind::Member
                && candidate.context_count == 0
        });
        let callables = crate::libraries::Callables::Functions(functions);
        let crate::symbol_resolver::CandidateSelection::Selected((selected, params, Ty::Int)) =
            self.resolver()
                .select_receiver_function_with_params_tracking(
                    comparable,
                    "compareTo",
                    &[crate::symbol_resolver::CallArgKind::Typed(element)],
                    &[],
                    &callables,
                    None,
                )
        else {
            return;
        };
        self.resolved_operator_calls.insert(
            (expression, SyntheticOperatorCall::CompareTo),
            selected_member_call(comparable, &selected, params, Ty::Int),
        );
    }

    /// The `contains` operator call testing membership of `value` in a range of `range_ty`.
    fn range_contains_call(
        &mut self,
        scope: &CheckerScope<'_>,
        expression: ExprId,
        range_ty: Ty,
        value_ty: Ty,
        value: ExprId,
    ) -> Option<(Ty, ResolvedCall)> {
        self.operator_call_ret(
            scope,
            expression,
            range_ty,
            "contains",
            &[value_ty],
            &[value],
            self.span(expression),
            None,
        )
    }

    /// Select `contains` through a type parameter's checked primitive bound solely to authorize
    /// direct comparison. This still runs the ordinary shared operator selector; it changes only
    /// the semantic argument view. A non-direct fallback calls [`Self::range_contains_call`] with
    /// the original symbolic source type instead.
    fn range_contains_call_for_bound(
        &mut self,
        scope: &CheckerScope<'_>,
        expression: ExprId,
        range_ty: Ty,
        bound: Ty,
        value: ExprId,
    ) -> Option<(Ty, ResolvedCall)> {
        self.select_operator(
            scope,
            expression,
            range_ty,
            "contains",
            &[bound],
            &[value],
            self.span(expression),
            None,
            Some(vec![crate::symbol_resolver::CallArgKind::Typed(bound)]),
        )
        .map(|selected| (selected.ret, selected.call))
    }

    fn finish_range_contains(
        &mut self,
        operands: &InRangeOperands,
        range_ty: Ty,
        range_call: ResolvedCall,
        contains: Option<(Ty, ResolvedCall)>,
    ) -> Ty {
        let Some((Ty::Boolean, contains_call)) = contains else {
            self.diags.error(
                self.span(operands.expression),
                format!(
                    "operator 'contains' cannot be applied to range '{}' and '{}'",
                    range_ty.source_name(),
                    operands.value_ty.source_name()
                ),
            );
            return Ty::Error;
        };
        self.resolved_operator_calls.insert(
            (operands.expression, range_operator(operands.kind).key),
            range_call,
        );
        self.resolved_operator_calls.insert(
            (operands.expression, SyntheticOperatorCall::Contains),
            contains_call,
        );
        Ty::Boolean
    }
}

/// The call target of a selected member operator over `receiver`.
fn selected_member_call(
    receiver: Ty,
    selected: &crate::libraries::FunctionInfo,
    params: Vec<Ty>,
    ret: Ty,
) -> ResolvedCall {
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
}

/// `computeComparisonClass`: the widest numeric type of a range literal's bounds and the value.
fn range_comparison_type(start: Ty, end: Ty, value: Ty) -> Option<Ty> {
    widest_numeric_type(widest_numeric_type(start, end)?, value)
}

/// `leastCommonPrimitiveNumericType`.
fn widest_numeric_type(left: Ty, right: Ty) -> Option<Ty> {
    let either = |types: &[Ty]| types.contains(&left) || types.contains(&right);
    Some(if either(&[Ty::Double]) {
        Ty::Double
    } else if either(&[Ty::Float]) {
        Ty::Float
    } else if either(&[Ty::ULong]) {
        Ty::ULong
    } else if either(&[Ty::UInt, Ty::UShort, Ty::UByte]) {
        Ty::UInt
    } else if either(&[Ty::Long]) {
        Ty::Long
    } else if either(&[Ty::Int, Ty::Short, Ty::Byte]) {
        Ty::Int
    } else if either(&[Ty::Char]) {
        Ty::Char
    } else {
        return None;
    })
}

/// Whether the selected range declaration is one kotlinc's `RangeContainsLowering` turns into
/// comparisons: a closed range built by `rangeTo`, `rangeUntil` or `until` over progression
/// elements, or a floating-point `rangeTo`. `downTo` builds a progression, which is not a
/// `ClosedRange`, and a floating-point `rangeUntil` an `OpenEndRange`: membership in either calls
/// its `contains`.
fn selected_range_comparison_provenance(
    kind: crate::ast::RangeKind,
    range_call: &ResolvedCall,
) -> Option<crate::fir::FirRangeComparisonProvenance> {
    use crate::ast::RangeKind;
    use crate::fir::FirRangeComparisonProvenance as Provenance;
    use crate::libraries::{CompilerIntrinsic, MemberRealization};

    match (kind, range_call.range_provider_realization()?) {
        (RangeKind::Through, MemberRealization::RangeConstruction { open_end: false }) => {
            Some(Provenance::RangeConstruction { open_end: false })
        }
        (RangeKind::OpenEnd, MemberRealization::RangeConstruction { open_end: true }) => {
            Some(Provenance::RangeConstruction { open_end: true })
        }
        (RangeKind::Until, MemberRealization::Intrinsic(CompilerIntrinsic::RangeUntil)) => {
            Some(Provenance::RangeUntil)
        }
        (
            RangeKind::Through,
            MemberRealization::Intrinsic(CompilerIntrinsic::FloatingRangeMembership),
        ) => Some(Provenance::FloatingRangeMembership),
        _ => None,
    }
}

impl ResolvedCall {
    /// Provider realization attached to this exact selected range declaration. Normalized library
    /// extensions carry compiler intrinsics beside their ordinary dispatch realization, so fold
    /// those provider fields before deciding whether membership may bypass `contains`.
    fn range_provider_realization(&self) -> Option<crate::libraries::MemberRealization> {
        let realization = match self {
            Self::Member(resolved) => resolved.member.realization,
            Self::TopLevel(call) => call
                .callable
                .compiler_intrinsic
                .map(crate::libraries::MemberRealization::Intrinsic)
                .unwrap_or(call.callable.member_realization),
            Self::Companion(member) => member.realization,
            Self::Extension(extension) => extension
                .callable
                .compiler_intrinsic
                .map(crate::libraries::MemberRealization::Intrinsic)
                .unwrap_or(extension.callable.member_realization),
            Self::MemberExtension { .. } | Self::LocalFunction(_) => return None,
        };
        Some(realization)
    }
}

#[derive(Clone, Debug)]
pub(crate) struct ResolvedInRangeComparison {
    pub(crate) comparison: Ty,
    pub(crate) provenance: crate::fir::FirRangeComparisonProvenance,
    /// Exact provider-selected comparison for an unsigned counter. Common lowering consumes this
    /// identity before publishing the normalized unsigned-ordering operation to backends.
    pub(crate) unsigned_compare: Option<super::for_loop_iteration::RuntimeFunction>,
}

struct InRangeOperands {
    expression: ExprId,
    kind: crate::ast::RangeKind,
    value_ty: Ty,
}
