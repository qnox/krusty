//! The Kotlin `invoke` convention: `receiver(args)` resolved as a call to `operator fun invoke`.
//!
//! One entry point answers every spelling — a function-value receiver, a member `operator fun
//! invoke`, a member EXTENSION one reached through an implicit dispatch receiver, and a companion's
//! factory `invoke` — so a caller never has to know which of them answered. Lambda shaping for the
//! companion spelling lives here too, because it reads the same overload family the selection does.
//!
//! Every one of these paths takes the call's EXPECTED type as a constraint. A formal that appears
//! only in a lambda's parameter, or in no parameter at all, has no other source, and the expectation
//! must reach the selection BEFORE the lambda body is typed or the body resolves against nothing. A
//! safe call lifts one nullable layer first: `a?.invoke(…)` has type `R?` where `R` is the invoke's
//! own result, so an expected `R?` constrains `R`.

use super::*;

impl Checker<'_> {
    /// Lambda shape for a `Type(args) { … }` factory call answered by a semantic companion's
    /// `operator fun invoke`. The companion's callable metadata enters the same mapping,
    /// applicability, binding, and shape operations as every other provider overload.
    pub(super) fn companion_invoke_lambda_shape(
        &self,
        scope: &CheckerScope<'_>,
        name: &str,
        args_and_partial: (&[ExprId], &[Option<Ty>]),
        // How the arguments were WRITTEN: their labels, and whether the last one is a trailing
        // lambda. Both feed the same parameter mapping, so they travel together.
        written_arguments: (Option<&[Option<String>]>, bool),
        type_args: &[Ty],
        result_constraint: CallResultConstraint,
    ) -> Option<crate::symbol_resolver::LambdaCallShape> {
        let (args, arg_tys) = args_and_partial;
        let (arg_names, trailing_lambda) = written_arguments;
        let overloads = self.companion_invoke_overloads(scope, name);
        for overload in &overloads {
            if overload.is_extension() {
                continue;
            }
            let Some(argument_map) = call_argument_parameter_indices(
                arg_tys.len(),
                overload.semantic_params().len(),
                arg_names,
                trailing_lambda,
                &overload.call_sig,
            ) else {
                continue;
            };
            if !type_args.is_empty()
                && overload.semantic_signature().formals.len() != type_args.len()
            {
                continue;
            }
            if !self.lambda_overload_partially_applicable(
                overload,
                None,
                (args, arg_tys),
                &argument_map,
                &named_whole_array_varargs(&argument_map, arg_names, &overload.call_sig),
                type_args,
            ) {
                continue;
            }
            if let Some(shape) = self.lambda_shape_for_overload(
                overload,
                None,
                (args, arg_tys),
                (
                    &argument_map,
                    &named_whole_array_varargs(&argument_map, arg_names, &overload.call_sig),
                ),
                type_args,
                result_constraint,
            ) {
                return Some(shape);
            }
        }
        None
    }

    /// Select the Kotlin invoke-operator convention for `receiver(args)`. One entry point covers both
    /// a function-value receiver (`Ty::Fun`) and a non-function receiver with a member `operator fun
    /// invoke`, recording a single [`ExprLowering::Invoke`] and returning the call's result type.
    /// Candidate discovery and overload selection remain explicit in the result: callers that are
    /// still walking a callable tower can retain a rejected level without resolving it again.
    pub(super) fn record_invoke(
        &mut self,
        scope: &CheckerScope<'_>,
        call_args: CallArgs<'_>,
        receiver: ExprId,
        receiver_ty: Ty,
        span: Span,
        result_constraint: CallResultConstraint,
    ) -> InvokeResolution {
        let CallArgs {
            call,
            args,
            arg_tys,
        } = call_args;
        let semantic_receiver_ty = self
            .expression_function_type(scope, receiver, receiver_ty)
            .unwrap_or(receiver_ty);
        crate::trace_compiler!(
            "resolve",
            "invoke selection call={call:?} receiver={receiver:?} nominal={receiver_ty:?} semantic={semantic_receiver_ty:?} args={arg_tys:?}",
        );
        let (params, ret, kind, arguments_already_mapped) = match semantic_receiver_ty {
            Ty::Fun(sig) => {
                let context_count = sig.context_count.min(sig.params.len());
                // Context-function values support both invocation forms: callers may pass the
                // context slots explicitly (`f(context, value)`), or omit the leading slots and let
                // ordinary context lookup supply them (`with(context) { f(value) }`).
                let explicit_context = context_count > 0 && arg_tys.len() == sig.params.len();
                if context_count > 0 && !explicit_context {
                    let Some(sources) =
                        self.select_context_arguments(scope, &sig.params[..context_count])
                    else {
                        self.diags.error(
                            span,
                            "no implicit value is available for the context parameters".to_string(),
                        );
                        return InvokeResolution::Selected(Ty::Error);
                    };
                    self.context_args.insert(call, sources);
                }
                (
                    if explicit_context {
                        sig.params.clone()
                    } else {
                        sig.params[context_count..].to_vec()
                    },
                    sig.ret,
                    InvokeKind::Function {
                        context_params: if explicit_context {
                            Vec::new()
                        } else {
                            sig.params[..context_count].to_vec()
                        },
                        ret: sig.ret,
                        suspend: sig.suspend,
                    },
                    false,
                )
            }
            _ => {
                let explicit_type_args = self.resolved_explicit_type_args(scope, call);
                let invoke_candidates = self.invoke_operator_candidates(receiver_ty);
                let overloads = invoke_candidates
                    .iter()
                    .filter(|candidate| candidate.kind == crate::libraries::FnKind::Member)
                    .cloned()
                    .collect::<Vec<_>>();
                let member_selection = self.select_callable_candidate(
                    scope,
                    CallArgs {
                        call,
                        args,
                        arg_tys,
                    },
                    &explicit_type_args,
                    None,
                    result_constraint,
                    overloads.clone(),
                );
                if let Some(CallableCandidateSelection::Ambiguous(candidates)) = &member_selection {
                    return InvokeResolution::Ambiguous(candidates.clone());
                }
                if let Some(selected) =
                    member_selection.and_then(CallableCandidateSelection::available)
                {
                    // Generic invoke selection owns the same substitution handoff as every other
                    // generic call. Checked FIR needs these bindings both to specialize the stable
                    // target signature and to prevent a callee-owned `T` from leaking into a
                    // non-generic caller (`invoke<T>(() -> T)` with a `Nothing` lambda).
                    if let Some(signature) = selected.generic_sig.as_ref() {
                        let resolved = signature
                            .formals
                            .iter()
                            .map(|formal| selected.bindings.get(formal).copied())
                            .collect::<Vec<_>>();
                        if !resolved.is_empty() && resolved.iter().all(Option::is_some) {
                            self.resolved_call_type_args.insert(call, resolved);
                        }
                    }
                    let argument_names = self.file.call_arg_names.get(&call.0).map(Vec::as_slice);
                    let Some(shape) = self.contextual_call_shape(
                        scope,
                        &selected.applied_params(),
                        &selected.call_sig,
                        selected.context_count,
                        argument_names,
                    ) else {
                        return InvokeResolution::Inapplicable(overloads);
                    };
                    if !self.expect_selected_call_args(
                        scope,
                        CallArgs {
                            call,
                            args,
                            arg_tys,
                        },
                        &shape.params,
                        &shape.call_sig,
                        None,
                    ) {
                        return InvokeResolution::Selected(Ty::Error);
                    }
                    let context_sources = shape
                        .context_sources
                        .iter()
                        .flatten()
                        .cloned()
                        .collect::<Vec<_>>();
                    if !context_sources.is_empty() {
                        self.context_args.insert(call, context_sources.clone());
                        self.mark_context_extension_receiver_used(scope, call, &context_sources);
                    }
                    let ret = selected.callable.ret;
                    let origin = selected.callable.origin.clone();
                    let physical_params = selected.callable.physical_params.clone();
                    let member = selected.member_with_return(ret);
                    let access_probe = crate::symbol_resolver::ResolvedMember {
                        receiver: receiver_ty,
                        member: member.clone(),
                        physical_params: physical_params.clone(),
                        context_args: shape.context_sources.clone(),
                        ret,
                        projected_return_hazard: selected.projected_return_hazard,
                        suspend: selected.flags.suspend,
                        origin: origin.clone(),
                    };
                    let owner = member
                        .owner
                        .expect("a selected member invoke has a declaring classifier");
                    if !self.selected_member_accessible(&access_probe, owner) {
                        self.reject_if_inaccessible(
                            member.visibility,
                            &member.name,
                            owner,
                            self.call_callee_name_span(call),
                        );
                        return InvokeResolution::Selected(Ty::Error);
                    }
                    let defaults = selected.default_values.clone();
                    let target = ResolvedCall::Member(access_probe);
                    (
                        shape.params,
                        ret,
                        InvokeKind::Operator {
                            receiver_ty,
                            target: Box::new(target),
                            param_default_values: defaults,
                        },
                        true,
                    )
                } else if let Some(ret) = self.check_member_extension_function_call_mode(
                    scope,
                    CallArgs {
                        call,
                        args,
                        arg_tys,
                    },
                    receiver_ty,
                    CALLABLE_INVOKE_OPERATOR,
                    (MemberExtensionSelection::Operators, None),
                    result_constraint,
                ) {
                    // A member EXTENSION `operator fun Recv.invoke(...)` on an implicit receiver
                    // (`"case" { … }` inside a receiver-DSL lambda). Keep the selected member-
                    // extension target and publish the same invoke handoff every other invoke
                    // convention uses; lowering must not rediscover this call from its name shape.
                    let target = self
                        .resolved_calls
                        .get(&call)
                        .cloned()
                        .expect("selected member-extension invoke target was recorded");
                    let params = match &target {
                        ResolvedCall::MemberExtension {
                            params,
                            context_args,
                            ..
                        } => params
                            .get(context_args.len()..)
                            .unwrap_or_default()
                            .to_vec(),
                        _ => unreachable!("member-extension invoke recorded a different target"),
                    };
                    (
                        params,
                        ret,
                        InvokeKind::Operator {
                            receiver_ty,
                            target: Box::new(target),
                            param_default_values: Vec::new(),
                        },
                        true,
                    )
                } else {
                    let extensions = invoke_candidates
                        .into_iter()
                        // The call convention is semantic: a same-named non-operator extension remains
                        // callable explicitly as `receiver.invoke(...)`, but never as `receiver(...)`.
                        // `FnFlags::operator` is populated uniformly from source and classpath metadata.
                        // Keep compatible supertypes too: `operator fun Interface.invoke()` applies
                        // to a value whose nominal type implements `Interface`. Receiver rank still
                        // participates in ordinary overload selection and chooses the nearest
                        // applicable extension; requiring rank zero incorrectly meant exact-type only.
                        .filter(|o| o.is_extension())
                        .collect::<Vec<_>>();
                    let extension_selection = self.select_callable_candidate(
                        scope,
                        CallArgs {
                            call,
                            args,
                            arg_tys,
                        },
                        &[],
                        Some(receiver_ty),
                        result_constraint,
                        extensions.clone(),
                    );
                    if let Some(CallableCandidateSelection::Ambiguous(candidates)) =
                        &extension_selection
                    {
                        return InvokeResolution::Ambiguous(candidates.clone());
                    }
                    let Some(selected) =
                        extension_selection.and_then(CallableCandidateSelection::available)
                    else {
                        return if overloads.is_empty() {
                            if extensions.is_empty() {
                                InvokeResolution::Absent
                            } else {
                                InvokeResolution::Inapplicable(extensions)
                            }
                        } else {
                            let mut rejected = overloads;
                            rejected.extend(extensions);
                            InvokeResolution::Inapplicable(rejected)
                        };
                    };
                    let argument_names = self.file.call_arg_names.get(&call.0).map(Vec::as_slice);
                    let Some(shape) = self.contextual_call_shape(
                        scope,
                        &selected.applied_params(),
                        &selected.call_sig,
                        selected.context_count,
                        argument_names,
                    ) else {
                        return InvokeResolution::Inapplicable(extensions);
                    };
                    if !self.expect_selected_call_args(
                        scope,
                        CallArgs {
                            call,
                            args,
                            arg_tys,
                        },
                        &shape.params,
                        &shape.call_sig,
                        None,
                    ) {
                        return InvokeResolution::Selected(Ty::Error);
                    }
                    let context_args = shape
                        .context_sources
                        .iter()
                        .flatten()
                        .cloned()
                        .collect::<Vec<_>>();
                    if !context_args.is_empty() {
                        self.context_args.insert(call, context_args.clone());
                        self.mark_context_extension_receiver_used(scope, call, &context_args);
                    }
                    let ret = selected.callable.ret;
                    let target =
                        if selected.stable_declaration.is_some() || selected.source_key.is_some() {
                            self.resolved_source_extension_call(
                                &selected,
                                receiver_ty,
                                ret,
                                context_args,
                                selected.default_values.clone(),
                            )
                        } else {
                            ResolvedCall::library_extension(selected.callable.clone())
                        };
                    (
                        shape.params,
                        ret,
                        InvokeKind::Operator {
                            receiver_ty,
                            target: Box::new(target),
                            param_default_values: selected.default_values.clone(),
                        },
                        true,
                    )
                }
            }
        };
        // `expect_selected_call_args` has already validated and recorded a selected callable's
        // named/default/vararg mapping. Function values still use exact positional validation.
        if !arguments_already_mapped && params.len() != arg_tys.len() {
            self.diags.error(
                span,
                format!(
                    "invoke operator expects {} args, got {}",
                    params.len(),
                    arg_tys.len()
                ),
            );
            return InvokeResolution::Selected(ret);
        }
        if !arguments_already_mapped {
            let positional_signature = CallSig::default();
            let implicit_lambda_label =
                call_implicit_lambda_label(self.file, call).map(str::to_string);
            for (i, (p, a)) in params.iter().zip(arg_tys).enumerate() {
                let actual = self.selected_argument_type(
                    scope,
                    args[i],
                    *a,
                    *p,
                    &positional_signature,
                    i,
                    implicit_lambda_label.as_deref(),
                );
                self.expect_call_arg(scope, *p, args[i], actual);
            }
        }
        self.expr_lowers.insert(
            call,
            ExprLowering::Invoke {
                receiver,
                params,
                kind,
            },
        );
        InvokeResolution::Selected(ret)
    }

    pub(super) fn record_invoke_or_report(
        &mut self,
        scope: &CheckerScope<'_>,
        call_args: CallArgs<'_>,
        receiver: ExprId,
        receiver_ty: Ty,
        span: Span,
        result_constraint: CallResultConstraint,
    ) -> Option<Ty> {
        match self.record_invoke(
            scope,
            call_args,
            receiver,
            receiver_ty,
            span,
            result_constraint,
        ) {
            InvokeResolution::Selected(ret) => Some(ret),
            InvokeResolution::Ambiguous(candidates) => {
                self.diags.error(
                    span,
                    self.ambiguous_member_candidates_message(CALLABLE_INVOKE_OPERATOR, &candidates),
                );
                Some(Ty::Error)
            }
            InvokeResolution::Inapplicable(candidates) => {
                self.diags.error(
                    span,
                    self.inapplicable_member_candidates_message(
                        CALLABLE_INVOKE_OPERATOR,
                        &candidates,
                    ),
                );
                Some(Ty::Error)
            }
            InvokeResolution::Absent => None,
        }
    }

    /// Resolve the companion `invoke` overload family once for both lambda shaping and constructor
    /// arbitration. This prevents those consumers from growing separate origin/name lookup rules.
    fn companion_invoke_overloads(
        &self,
        scope: &CheckerScope<'_>,
        name: &str,
    ) -> Vec<crate::libraries::FunctionInfo> {
        let Ok(ResolvedQualifier::Classifier(internal)) =
            self.qualifier(scope, QualifierInput::Root(name))
        else {
            return Vec::new();
        };
        let Some(companion) = self
            .resolved_type_name(internal)
            .and_then(|classifier| classifier.companion_object.as_ref().map(|(_, ty)| *ty))
            .map(Ty::obj_name)
        else {
            return Vec::new();
        };
        self.resolver()
            .resolve_symbol(
                crate::symbol_resolver::SymRecv::Value(companion),
                CALLABLE_INVOKE_OPERATOR,
                &[],
                &[],
            )
            .map(crate::symbol_resolver::Symbol::overloads)
            .unwrap_or_default()
    }
}
