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

/// One property read and the exact `invoke` declaration selected for its value before contextual
/// lambda checking. The call keeps this object until commitment, so completed lambda types cannot
/// reopen the overload family and replace the declaration that supplied their expectations.
pub(super) struct PropertyInvokePlan {
    pub(super) property: PropertyReadSelection,
    pub(super) invoke: SelectedInvokePlan,
}

pub(super) struct SelectedInvokePlan {
    target: SelectedInvokeTarget,
    /// Declaration parameter for each source argument, after named/default/trailing-lambda mapping.
    argument_parameters: Vec<usize>,
    /// Exact permission for each source argument, from the selected declaration and its mapping.
    /// A missing modifier remains unknown rather than granting a non-local return.
    argument_inlining: Vec<Option<bool>>,
    pub(super) lambda_shape: crate::symbol_resolver::LambdaCallShape,
}

enum SelectedInvokeTarget {
    Callable {
        declaration: crate::libraries::FunctionInfo,
        /// `None` selects a member; `Some` is the semantic receiver of a top-level extension.
        extension_receiver: Option<Ty>,
    },
    MemberExtension(Box<MemberExtensionFunctionShape>),
}

struct SelectedInvokePlanRequest<'a> {
    call: ExprId,
    args: &'a [ExprId],
    partial: &'a [Option<Ty>],
    receiver_ty: Ty,
    extension_receiver: Option<Ty>,
    result_constraint: CallResultConstraint,
    declaration: crate::libraries::FunctionInfo,
}

impl SelectedInvokePlan {
    /// Exact permission attached to the mapped selected parameter. Missing modifier metadata stays
    /// unknown: an inline call never turns absence into the permissive `None` modifier.
    pub(super) fn inlines_argument(&self, argument: usize) -> Option<bool> {
        self.argument_inlining.get(argument).copied().flatten()
    }
}

impl Checker<'_> {
    /// Select the property and its value's invoke operator once, while non-contextual arguments are
    /// already typed and lambda slots are still postponed. Only an exact overload produces a plan;
    /// ambiguity or incomplete declaration facts remain non-permissive and are diagnosed by the
    /// ordinary final call path.
    pub(super) fn plan_property_invoke(
        &mut self,
        scope: &CheckerScope<'_>,
        call: ExprId,
        receiver: Ty,
        name: &str,
        args: &[ExprId],
        partial: &[Option<Ty>],
        result_constraint: CallResultConstraint,
    ) -> Option<PropertyInvokePlan> {
        if !args
            .iter()
            .any(|argument| matches!(self.file.expr(*argument), Expr::Lambda { .. }))
        {
            return None;
        }
        let property = self.select_property_read(scope, receiver, name).ok()??;
        let value = self.declared_function_semantic_type(property.ty());
        if matches!(value.non_null(), Ty::Fun(_)) {
            return None;
        }
        let invoke =
            self.plan_selected_invoke(scope, call, args, partial, value, result_constraint)?;
        Some(PropertyInvokePlan { property, invoke })
    }

    fn plan_selected_invoke(
        &mut self,
        scope: &CheckerScope<'_>,
        call: ExprId,
        args: &[ExprId],
        partial: &[Option<Ty>],
        receiver_ty: Ty,
        result_constraint: CallResultConstraint,
    ) -> Option<SelectedInvokePlan> {
        let explicit_type_args = self.resolved_explicit_type_args(scope, call);
        let provisional = args
            .iter()
            .copied()
            .zip(partial.iter().copied())
            .map(|(argument, ty)| {
                ty.unwrap_or_else(|| self.invoke_plan_argument_probe(scope, argument))
            })
            .collect::<Vec<_>>();
        let candidates = self.invoke_operator_candidates(receiver_ty);
        let members = candidates
            .iter()
            .filter(|candidate| candidate.kind == crate::libraries::FnKind::Member)
            .cloned()
            .collect::<Vec<_>>();
        if let Some(selection) = self.select_callable_candidate(
            scope,
            CallArgs {
                call,
                args,
                arg_tys: &provisional,
            },
            &explicit_type_args,
            None,
            result_constraint,
            members.clone(),
        ) {
            return match selection {
                CallableCandidateSelection::Selected(selected) => self.selected_invoke_plan(
                    scope,
                    SelectedInvokePlanRequest {
                        call,
                        args,
                        partial,
                        receiver_ty,
                        extension_receiver: None,
                        result_constraint,
                        declaration: members.get(selected.candidate_index?)?.clone(),
                    },
                ),
                CallableCandidateSelection::MissingContext(_)
                | CallableCandidateSelection::Ambiguous(_) => None,
            };
        }

        let partial_provisional = provisional.iter().copied().map(Some).collect::<Vec<_>>();
        let mut member_extensions = self
            .member_extension_function_shapes(scope, receiver_ty, CALLABLE_INVOKE_OPERATOR)
            .into_iter()
            .filter(|shape| shape.is_operator)
            .filter_map(|shape| {
                let instantiated = self.instantiate_member_extension_constrained(
                    scope,
                    &shape,
                    MemberExtensionCall {
                        extension_receiver: receiver_ty,
                        name: CALLABLE_INVOKE_OPERATOR,
                        args,
                        partial_arg_tys: &partial_provisional,
                        arg_names: self.file.call_arg_names.get(&call.0).map(Vec::as_slice),
                        explicit_type_args: &explicit_type_args,
                        trailing_lambda: self.file.call_has_trailing_lambda.contains(&call.0),
                    },
                    result_constraint,
                )?;
                Some((shape, instantiated))
            })
            .collect::<Vec<_>>();
        if !member_extensions.is_empty() {
            let mut maximal = maximal_member_extensions(self, &member_extensions, |candidate| {
                candidate.0.priority
            });
            let best = maximal
                .iter()
                .map(|index| member_extensions[*index].1.score)
                .max()
                .unwrap_or_default();
            maximal.retain(|index| member_extensions[*index].1.score == best);
            let [selected] = maximal.as_slice() else {
                return None;
            };
            let (shape, instantiated) = member_extensions.swap_remove(*selected);
            return self.selected_member_extension_invoke_plan(args, shape, instantiated);
        }

        let extensions = candidates
            .into_iter()
            .filter(crate::libraries::FunctionInfo::is_extension)
            .collect::<Vec<_>>();
        let selected = self
            .select_callable_candidate(
                scope,
                CallArgs {
                    call,
                    args,
                    arg_tys: &provisional,
                },
                &explicit_type_args,
                Some(receiver_ty),
                result_constraint,
                extensions.clone(),
            )?
            .available()?;
        self.selected_invoke_plan(
            scope,
            SelectedInvokePlanRequest {
                call,
                args,
                partial,
                receiver_ty,
                extension_receiver: Some(receiver_ty),
                result_constraint,
                declaration: extensions.get(selected.candidate_index?)?.clone(),
            },
        )
    }

    fn selected_member_extension_invoke_plan(
        &self,
        args: &[ExprId],
        shape: MemberExtensionFunctionShape,
        instantiated: InstantiatedMemberExtension,
    ) -> Option<SelectedInvokePlan> {
        let mut mapped = vec![None; args.len()];
        for &(parameter, source) in &instantiated.argument_parameters {
            *mapped.get_mut(source)? = Some(parameter);
        }
        let argument_parameters = mapped.into_iter().collect::<Option<Vec<_>>>()?;
        let inline = InlineKind::from_flags(
            shape.function.signature.is_inline(),
            shape.function.signature.requires_splice(),
        );
        let argument_inlining = argument_parameters
            .iter()
            .map(|&parameter| {
                if !inline.can_inline() {
                    Some(false)
                } else {
                    instantiated
                        .call_sig
                        .inline_modifiers
                        .get(parameter)
                        .map(|modifier| modifier.runs_in_caller_frame())
                }
            })
            .collect::<Vec<_>>();
        let argument_types = argument_parameters
            .iter()
            .map(|&parameter| instantiated.visible_params.get(parameter).copied())
            .collect::<Option<Vec<_>>>()?;
        let mut param_types = vec![Vec::new(); args.len()];
        let mut expected_types = vec![None; args.len()];
        let mut receivers = vec![None; args.len()];
        let mut context_counts = vec![0; args.len()];
        for (source, (&parameter, &expected)) in
            argument_parameters.iter().zip(&argument_types).enumerate()
        {
            let Ty::Fun(signature) = expected.non_null() else {
                continue;
            };
            param_types[source] = signature.params.clone();
            expected_types[source] = Some(expected);
            context_counts[source] = signature.context_count;
            if instantiated
                .call_sig
                .lambda_receiver_params
                .get(parameter)
                .copied()
                .unwrap_or(false)
            {
                receivers[source] = signature.params.get(signature.context_count).copied();
            }
        }
        let boxes_captures = argument_parameters
            .iter()
            .map(|&parameter| {
                instantiated
                    .call_sig
                    .inline_modifiers
                    .get(parameter)
                    .map(|modifier| modifier.boxes_captures())
            })
            .collect::<Vec<_>>();
        Some(SelectedInvokePlan {
            target: SelectedInvokeTarget::MemberExtension(Box::new(shape)),
            argument_parameters,
            argument_inlining,
            lambda_shape: crate::symbol_resolver::LambdaCallShape {
                argument_parameters: argument_types,
                param_types: Some(param_types),
                expected_types: expected_types
                    .iter()
                    .any(Option::is_some)
                    .then_some(expected_types),
                receivers: receivers.iter().any(Option::is_some).then_some(receivers),
                context_counts: context_counts
                    .iter()
                    .any(|count| *count > 0)
                    .then_some(context_counts),
                boxes_captures: Some(boxes_captures),
                inline: inline.can_inline(),
                ..crate::symbol_resolver::LambdaCallShape::default()
            },
        })
    }

    /// Declaration-site lambda parameter annotations are candidate evidence before the body is
    /// checked. Keep the result postponed while exposing only those written inputs; an unannotated
    /// slot remains `Error` and cannot manufacture an overload preference.
    fn invoke_plan_argument_probe(&mut self, scope: &CheckerScope<'_>, argument: ExprId) -> Ty {
        let Some(declared) = self.file.lambda_param_types.get(&argument.0).cloned() else {
            return Ty::Error;
        };
        if declared.is_empty() || declared.iter().any(Option::is_none) {
            return Ty::Error;
        }
        let parameters = declared
            .into_iter()
            .flatten()
            .map(|reference| self.type_ref_ty(scope, &reference))
            .collect::<Vec<_>>();
        Ty::fun(parameters, Ty::Error)
    }

    fn selected_invoke_plan(
        &mut self,
        scope: &CheckerScope<'_>,
        request: SelectedInvokePlanRequest<'_>,
    ) -> Option<SelectedInvokePlan> {
        let SelectedInvokePlanRequest {
            call,
            args,
            partial,
            receiver_ty,
            extension_receiver,
            result_constraint,
            declaration,
        } = request;
        let argument_names = self.file.call_arg_names.get(&call.0).map(Vec::as_slice);
        let contextual = self.contextual_call_shape(
            scope,
            &declaration.semantic_params(),
            &declaration.call_sig,
            declaration.context_count,
            argument_names,
        )?;
        let visible_parameters = call_argument_parameter_indices(
            args.len(),
            contextual.params.len(),
            argument_names,
            self.file.call_has_trailing_lambda.contains(&call.0),
            &contextual.call_sig,
        )?;
        let argument_parameters = visible_parameters
            .into_iter()
            .map(|parameter| contextual.parameter_indices[parameter])
            .collect::<Vec<_>>();
        let whole_arrays =
            named_whole_array_varargs(&argument_parameters, argument_names, &declaration.call_sig);
        let explicit_type_args = self.resolved_explicit_type_args(scope, call);
        let lambda_shape = self.lambda_shape_for_overload(
            &declaration,
            extension_receiver,
            (args, partial),
            (&argument_parameters, &whole_arrays),
            &explicit_type_args,
            result_constraint,
        )?;
        crate::trace_compiler!(
            "resolve",
            "planned property invoke call={call:?} receiver={receiver_ty:?} owner={} descriptor={} argument_parameters={argument_parameters:?}",
            declaration.callable.owner,
            declaration.callable.descriptor,
        );
        let argument_inlining = argument_parameters
            .iter()
            .map(|&parameter| {
                if !declaration.flags.inline.can_inline() {
                    Some(false)
                } else {
                    declaration
                        .call_sig
                        .inline_modifiers
                        .get(parameter)
                        .map(|modifier| modifier.runs_in_caller_frame())
                }
            })
            .collect();
        Some(SelectedInvokePlan {
            target: SelectedInvokeTarget::Callable {
                declaration,
                extension_receiver,
            },
            argument_parameters,
            argument_inlining,
            lambda_shape,
        })
    }

    /// Complete generic bindings and record the target chosen by `plan`. Only that declaration is
    /// instantiated here; completed lambda results may refine its type arguments, but cannot reopen
    /// the overload family or change the argument mapping that supplied their return permission.
    pub(super) fn record_planned_invoke(
        &mut self,
        scope: &CheckerScope<'_>,
        call_args: CallArgs<'_>,
        receiver: ExprId,
        receiver_ty: Ty,
        plan: SelectedInvokePlan,
        result_constraint: CallResultConstraint,
    ) -> InvokeResolution {
        let CallArgs {
            call,
            args,
            arg_tys,
        } = call_args;
        let SelectedInvokePlan {
            target,
            argument_parameters,
            ..
        } = plan;
        let (declaration, extension_receiver) = match target {
            SelectedInvokeTarget::Callable {
                declaration,
                extension_receiver,
            } => (declaration, extension_receiver),
            SelectedInvokeTarget::MemberExtension(shape) => {
                return self.record_planned_member_extension_invoke(
                    scope,
                    CallArgs {
                        call,
                        args,
                        arg_tys,
                    },
                    (receiver, receiver_ty),
                    *shape,
                    &argument_parameters,
                    result_constraint,
                );
            }
        };
        let explicit_type_args = self.resolved_explicit_type_args(scope, call);
        let diagnostic_declaration = declaration.clone();
        let selection = self.select_callable_candidate(
            scope,
            CallArgs {
                call,
                args,
                arg_tys,
            },
            &explicit_type_args,
            extension_receiver,
            result_constraint,
            vec![declaration],
        );
        let selected = match selection {
            Some(CallableCandidateSelection::Selected(selected)) => *selected,
            Some(CallableCandidateSelection::MissingContext(_)) | None => {
                return InvokeResolution::Inapplicable(vec![diagnostic_declaration]);
            }
            Some(CallableCandidateSelection::Ambiguous(_)) => {
                unreachable!("one planned invoke declaration cannot become ambiguous")
            }
        };
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
            return InvokeResolution::Inapplicable(vec![diagnostic_declaration]);
        };
        let final_argument_parameters = call_argument_parameter_indices(
            args.len(),
            shape.params.len(),
            argument_names,
            self.file.call_has_trailing_lambda.contains(&call.0),
            &shape.call_sig,
        )
        .map(|parameters| {
            parameters
                .into_iter()
                .map(|parameter| shape.parameter_indices[parameter])
                .collect::<Vec<_>>()
        });
        if final_argument_parameters.as_deref() != Some(argument_parameters.as_slice()) {
            return InvokeResolution::Inapplicable(vec![diagnostic_declaration]);
        }
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
        let target = if extension_receiver.is_none() {
            let member = selected.member_with_return(ret);
            let access_probe = crate::symbol_resolver::ResolvedMember {
                receiver: receiver_ty,
                physical_params: selected.callable.physical_params.clone(),
                context_args: shape.context_sources.clone(),
                ret,
                projected_return_hazard: selected.projected_return_hazard,
                suspend: selected.flags.suspend,
                origin: selected.callable.origin.clone(),
                member: member.clone(),
            };
            let owner = member
                .owner
                .expect("a planned member invoke has a declaring classifier");
            if !self.selected_member_accessible(&access_probe, owner) {
                self.reject_if_inaccessible(
                    member.visibility,
                    &member.name,
                    owner,
                    self.call_callee_name_span(call),
                );
                return InvokeResolution::Selected(Ty::Error);
            }
            ResolvedCall::Member(access_probe)
        } else if selected.stable_declaration.is_some() || selected.source_key.is_some() {
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
        self.expr_lowers.insert(
            call,
            ExprLowering::Invoke {
                receiver,
                params: shape.params,
                kind: InvokeKind::Operator {
                    receiver_ty,
                    target: Box::new(target),
                    param_default_values: selected.default_values.clone(),
                },
            },
        );
        InvokeResolution::Selected(ret)
    }

    fn record_planned_member_extension_invoke(
        &mut self,
        scope: &CheckerScope<'_>,
        call_args: CallArgs<'_>,
        receiver: (ExprId, Ty),
        shape: MemberExtensionFunctionShape,
        argument_parameters: &[usize],
        result_constraint: CallResultConstraint,
    ) -> InvokeResolution {
        let (receiver, receiver_ty) = receiver;
        let CallArgs {
            call,
            args,
            arg_tys,
        } = call_args;
        let explicit_type_args = self.resolved_explicit_type_args(scope, call);
        let final_types = arg_tys.iter().copied().map(Some).collect::<Vec<_>>();
        let Some(instantiated) = self.instantiate_member_extension_constrained(
            scope,
            &shape,
            MemberExtensionCall {
                extension_receiver: receiver_ty,
                name: CALLABLE_INVOKE_OPERATOR,
                args,
                partial_arg_tys: &final_types,
                arg_names: self.file.call_arg_names.get(&call.0).map(Vec::as_slice),
                explicit_type_args: &explicit_type_args,
                trailing_lambda: self.file.call_has_trailing_lambda.contains(&call.0),
            },
            result_constraint,
        ) else {
            self.diags.error(
                self.call_callee_name_span(call),
                "the selected invoke operator is not applicable to the completed arguments"
                    .to_string(),
            );
            return InvokeResolution::Selected(Ty::Error);
        };
        let candidate = member_extension_selection::candidate(&shape, instantiated);
        let mut final_mapping = vec![None; args.len()];
        for &(parameter, source) in &candidate.argument_parameters {
            let Some(slot) = final_mapping.get_mut(source) else {
                return InvokeResolution::Selected(Ty::Error);
            };
            *slot = Some(parameter);
        }
        if final_mapping
            .into_iter()
            .collect::<Option<Vec<_>>>()
            .as_deref()
            != Some(argument_parameters)
        {
            self.diags.error(
                self.call_callee_name_span(call),
                "the selected invoke operator changed its argument mapping".to_string(),
            );
            return InvokeResolution::Selected(Ty::Error);
        }
        for &(parameter, source) in &candidate.argument_parameters {
            let (Some(&expected), Some(&actual)) =
                (candidate.visible_params.get(parameter), arg_tys.get(source))
            else {
                continue;
            };
            if let Some(coerced) = self.unit_coerced_lambda_type(args[source], expected, actual) {
                self.set(args[source], coerced);
            }
        }
        if candidate.visibility != Visibility::Public
            && self.reject_if_inaccessible(
                candidate.visibility,
                CALLABLE_INVOKE_OPERATOR,
                candidate.owner,
                self.call_callee_name_span(call),
            )
        {
            return InvokeResolution::Selected(Ty::Error);
        }
        let mut validation_params = candidate.visible_params.clone();
        if let Some(vararg) = candidate.call_sig.vararg_index {
            if let Some(element) = validation_params.get_mut(vararg) {
                *element = Ty::array(*element);
            }
        }
        if !self.expect_selected_call_args(
            scope,
            CallArgs {
                call,
                args,
                arg_tys,
            },
            &validation_params,
            &candidate.call_sig,
            None,
        ) {
            return InvokeResolution::Selected(Ty::Error);
        }
        let interface = self
            .resolver()
            .classifier(candidate.owner)
            .is_some_and(|classifier| classifier.is_interface());
        let dispatch_receiver = self.implicit_receiver_selection(candidate.dispatch_receiver);
        let target = candidate.resolved_call(dispatch_receiver, receiver_ty, interface);
        self.resolved_calls.insert(call, target.clone());
        self.mark_extension_receiver_used(call, candidate.dispatch_receiver);
        let params = candidate
            .params
            .get(candidate.context_args.len()..)
            .unwrap_or_default()
            .to_vec();
        let ret = candidate.ret;
        self.expr_lowers.insert(
            call,
            ExprLowering::Invoke {
                receiver,
                params,
                kind: InvokeKind::Operator {
                    receiver_ty,
                    target: Box::new(target),
                    param_default_values: Vec::new(),
                },
            },
        );
        InvokeResolution::Selected(ret)
    }

    pub(super) fn record_planned_invoke_or_report(
        &mut self,
        scope: &CheckerScope<'_>,
        call_args: CallArgs<'_>,
        receiver: ExprId,
        receiver_ty: Ty,
        span: Span,
        plan: SelectedInvokePlan,
        result_constraint: CallResultConstraint,
    ) -> Ty {
        match self.record_planned_invoke(
            scope,
            call_args,
            receiver,
            receiver_ty,
            plan,
            result_constraint,
        ) {
            InvokeResolution::Selected(ret) => ret,
            InvokeResolution::Inapplicable(candidates) => {
                self.diags.error(
                    span,
                    self.inapplicable_member_candidates_message(
                        CALLABLE_INVOKE_OPERATOR,
                        &candidates,
                    ),
                );
                Ty::Error
            }
            InvokeResolution::Ambiguous(_) | InvokeResolution::Absent => {
                unreachable!("a stable invoke plan has exactly one declaration")
            }
        }
    }

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
