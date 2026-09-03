//! Delegated-property convention selection and checked semantic recording.
//!
//! This module owns the transition from one checked delegate expression to the exact
//! `provideDelegate`/`getValue`/`setValue` declarations consumed by FIR. It reuses ordinary
//! receiver callable collection, member-extension selection, applicability, and generic inference.

use super::*;

enum DelegateGetValueAttempt {
    Complete(Option<Ty>),
    RetryWithReceiver(Ty),
}

pub(super) fn select_delegate_operator_return(
    resolver: &crate::symbol_resolver::SymbolResolver,
    receiver: Ty,
    name: &str,
    args: &[Ty],
) -> Option<Ty> {
    match select_delegate_operator(resolver, receiver, name, args) {
        crate::symbol_resolver::CandidateSelection::Selected((_, ret)) => Some(ret),
        crate::symbol_resolver::CandidateSelection::None
        | crate::symbol_resolver::CandidateSelection::Ambiguous => None,
    }
}

/// Select one delegated-property convention from the normalized member/extension family. Operator
/// filtering precedes applicability, so a same-named ordinary function cannot displace the
/// convention declaration.
pub(super) fn select_delegate_operator(
    resolver: &crate::symbol_resolver::SymbolResolver,
    receiver: Ty,
    name: &str,
    args: &[Ty],
) -> crate::symbol_resolver::CandidateSelection<(crate::libraries::FunctionInfo, Ty)> {
    let callables = resolver.receiver_callables(receiver, name);
    crate::trace_compiler!(
        "resolve",
        "delegate operator candidates receiver={receiver:?} name={name} candidates={:?}",
        callables
            .functions()
            .iter()
            .map(|candidate| (
                candidate.kind,
                candidate.flags.operator,
                candidate.flags.inline,
                candidate.semantic_params(),
                candidate.callable.ret,
            ))
            .collect::<Vec<_>>(),
    );
    let overloads = callables
        .functions()
        .iter()
        .filter(|candidate| candidate.flags.operator)
        .cloned()
        .collect::<Vec<_>>();
    let callables =
        crate::libraries::Callables::Functions(crate::libraries::FunctionSet { overloads });
    let args = args
        .iter()
        .copied()
        .map(CallArgKind::Typed)
        .collect::<Vec<_>>();
    match resolver.select_receiver_function_with_params_tracking(
        receiver,
        name,
        &args,
        &[],
        &callables,
    ) {
        crate::symbol_resolver::CandidateSelection::Selected((selected, _, ret)) => {
            crate::symbol_resolver::CandidateSelection::Selected((selected, ret))
        }
        crate::symbol_resolver::CandidateSelection::None => {
            crate::symbol_resolver::CandidateSelection::None
        }
        crate::symbol_resolver::CandidateSelection::Ambiguous => {
            crate::symbol_resolver::CandidateSelection::Ambiguous
        }
    }
}

impl Checker<'_> {
    /// Check a delegate expression and select its convention as one contextual operation.
    ///
    /// Generic delegate factories can receive their type argument only from `getValue`'s result.
    /// The expectation-free attempt is postponed: it may establish a constructor/call shape, but
    /// it does not own the final inference decision. Once `getValue` supplies a receiver
    /// expectation, the expression is checked authoritatively against that type.
    pub(super) fn check_delegate_getvalue(
        &mut self,
        scope: &CheckerScope<'_>,
        delegate: ExprId,
        provide_ref: Ty,
        this_ref: Ty,
        expected_property: Option<Ty>,
    ) -> (Ty, Option<Ty>) {
        let mut receiver_expectation = None;
        loop {
            let delegate_ty = match (receiver_expectation, expected_property) {
                (Some(expected), _) => self.expr_expected(scope, delegate, expected),
                // With no declared property type there is no later result expectation to commit a
                // postponed delegate expression. Its own arguments must therefore finish its type:
                // `val p by ReadOnlyProperty { _, kProperty -> kProperty }` infers the SAM result
                // (and hence `p`) as `KProperty<*>` from the lambda body.
                (None, None) => self.expr(scope, delegate),
                (None, Some(_)) => self.check_postponed_argument(scope, delegate),
            };
            match self.select_delegate_getvalue_attempt(
                scope,
                delegate,
                delegate_ty,
                provide_ref,
                this_ref,
                expected_property,
                receiver_expectation,
            ) {
                DelegateGetValueAttempt::Complete(result) => {
                    return (delegate_ty, result);
                }
                DelegateGetValueAttempt::RetryWithReceiver(expected) => {
                    receiver_expectation = Some(expected);
                }
            }
        }
    }

    fn select_delegate_getvalue_attempt(
        &mut self,
        scope: &CheckerScope<'_>,
        delegate: ExprId,
        delegate_ty: Ty,
        provide_ref: Ty,
        this_ref: Ty,
        expected_property: Option<Ty>,
        receiver_expectation: Option<Ty>,
    ) -> DelegateGetValueAttempt {
        crate::trace_compiler!(
            "fir",
            "resolve delegate convention delegate={delegate:?} receiver={delegate_ty:?} provide_ref={provide_ref:?} this_ref={this_ref:?}",
        );
        let kproperty = Ty::obj("kotlin/reflect/KProperty");
        let provide_target = self.select_delegate_operator(
            scope,
            delegate,
            delegate_ty,
            "provideDelegate",
            &[provide_ref, kproperty],
            None,
        );
        let stored_ty = provide_target
            .as_ref()
            .map(DelegateGetValueTarget::ret)
            .unwrap_or(delegate_ty);
        let Some(target) = self.select_delegate_operator(
            scope,
            delegate,
            stored_ty,
            "getValue",
            &[this_ref, kproperty],
            expected_property,
        ) else {
            return DelegateGetValueAttempt::Complete(None);
        };
        if let Some(applied_receiver) = target.applied_receiver().filter(|receiver| {
            if *receiver == delegate_ty
                || receiver.obj_internal() != stored_ty.obj_internal()
                || stored_ty.obj_internal() != delegate_ty.obj_internal()
            {
                return false;
            }
            if delegate_ty.type_args().is_empty() && !receiver.type_args().is_empty() {
                return true;
            }
            let mut bindings = crate::symbol_resolver::GSigBinds::new();
            crate::symbol_resolver::unify_ty(delegate_ty, *receiver, &mut bindings);
            !bindings.is_empty()
                && crate::symbol_resolver::ty_subst_keep_unbound(delegate_ty, &bindings)
                    == *receiver
        }) {
            if applied_receiver != delegate_ty && receiver_expectation != Some(applied_receiver) {
                return DelegateGetValueAttempt::RetryWithReceiver(applied_receiver);
            }
        }
        let ret = target.ret();
        if let Some(expected) = expected_property.filter(|expected| *expected != Ty::Error) {
            let convention_receiver = target.receiver_constrained_by_result(expected);
            let delegate_receiver = match (&provide_target, convention_receiver) {
                (Some(provide), Some(stored)) => provide.receiver_constrained_by_result(stored),
                (None, constrained) => constrained,
                (Some(_), None) => None,
            };
            if let Some(refined) = delegate_receiver
                .and_then(|constraint| {
                    self.resolver()
                        .apply_raw_receiver_constraint(delegate_ty, constraint)
                })
                .map(|receiver| self.refine_delegate_receiver_to_bounds(receiver))
                .filter(|receiver| *receiver != delegate_ty)
            {
                if receiver_expectation != Some(refined) {
                    return DelegateGetValueAttempt::RetryWithReceiver(refined);
                }
            }
            let mut bindings = crate::symbol_resolver::GSigBinds::new();
            crate::symbol_resolver::unify_ty(ret, expected, &mut bindings);
            let refined = crate::symbol_resolver::ty_subst_keep_unbound(delegate_ty, &bindings);
            let refined = match refined {
                Ty::Obj(name, arguments) if arguments.iter().any(|argument| *argument == ret) => {
                    Ty::obj_args_name(
                        name,
                        &arguments
                            .iter()
                            .map(|argument| {
                                if *argument == ret {
                                    expected
                                } else {
                                    *argument
                                }
                            })
                            .collect::<Vec<_>>(),
                    )
                }
                other => other,
            };
            let refined = self.refine_delegate_receiver_to_bounds(refined);
            crate::trace_compiler!(
                "fir",
                "refine delegate from getValue result delegate={delegate:?} ret={ret:?} expected={expected:?} bindings={bindings:?} receiver={delegate_ty:?} refined={refined:?}",
            );
            if refined != delegate_ty {
                if receiver_expectation != Some(refined) {
                    return DelegateGetValueAttempt::RetryWithReceiver(refined);
                }
            }
        }
        crate::trace_compiler!(
            "fir",
            "selected delegate getValue delegate={delegate:?} receiver={stored_ty:?} target={target:?}",
        );
        if let Some(provide_target) = provide_target {
            self.delegate_provide_targets
                .insert(delegate, provide_target);
        }
        self.delegate_getvalue_targets.insert(delegate, target);
        DelegateGetValueAttempt::Complete(Some(ret))
    }

    /// A delegated-result constraint can expose a nullable result around a non-null classifier
    /// variable (`getValue(): T?`, `T : Any`). A provisional unconstrained constructor probe may
    /// have completed that variable as `Any?`; replacing it with the whole expected nullable result
    /// would preserve the same bound violation. Normalize only that nullable shell against the
    /// declaration's actual upper bound, then let the authoritative expected constructor check
    /// validate every source argument again.
    fn refine_delegate_receiver_to_bounds(&self, receiver: Ty) -> Ty {
        let Ty::Obj(owner, arguments) = receiver else {
            return receiver;
        };
        let Some(class) = self.resolved_type_name(owner) else {
            return receiver;
        };
        let mut refined = arguments.to_vec();
        let mut bindings = HashMap::new();
        for (index, formal) in class.type_params().iter().enumerate() {
            let Some(argument) = refined.get(index).copied() else {
                break;
            };
            let bound = class
                .type_param_bounds()
                .get(index)
                .and_then(|bounds| bounds.first())
                .copied()
                .filter(|bound| *bound != Ty::Error)
                .map(|bound| crate::symbol_resolver::ty_subst_keep_unbound(bound, &bindings));
            let argument = match bound {
                Some(bound)
                    if argument.is_nullable()
                        && !self.receiver_is_assignable(argument, bound)
                        && self.receiver_is_assignable(argument.non_null(), bound) =>
                {
                    argument.non_null()
                }
                _ => argument,
            };
            refined[index] = argument;
            bindings.insert(formal.clone(), argument);
        }
        Ty::obj_args_name(owner, &refined)
    }

    pub(super) fn record_delegate_setvalue(
        &mut self,
        scope: &CheckerScope<'_>,
        delegate: ExprId,
        _delegate_ty: Ty,
        this_ref: Ty,
        property_ty: Ty,
    ) -> Option<()> {
        let delegate_ty = self.expr_types[delegate.0 as usize];
        crate::trace_compiler!(
            "fir",
            "resolve delegate setValue delegate={delegate:?} receiver={delegate_ty:?} this_ref={this_ref:?} property={property_ty:?}",
        );
        let kproperty = Ty::obj("kotlin/reflect/KProperty");
        let stored_ty = self
            .delegate_provide_targets
            .get(&delegate)
            .map(DelegateGetValueTarget::ret)
            .unwrap_or(delegate_ty);
        let target = self.select_delegate_operator(
            scope,
            delegate,
            stored_ty,
            "setValue",
            &[this_ref, kproperty, property_ty],
            None,
        )?;
        crate::trace_compiler!("fir", "selected delegate setValue target={target:?}");
        self.delegate_setvalue_targets.insert(delegate, target);
        Some(())
    }

    fn select_delegate_operator(
        &self,
        scope: &CheckerScope<'_>,
        delegate: ExprId,
        delegate_ty: Ty,
        name: &str,
        args: &[Ty],
        expected_result: Option<Ty>,
    ) -> Option<DelegateGetValueTarget> {
        let resolver = self.resolver();
        let callables = resolver.receiver_callables(delegate_ty, name);
        let call_args = args
            .iter()
            .copied()
            .map(CallArgKind::Typed)
            .collect::<Vec<_>>();
        let select_kind = |kind| {
            let overloads = callables
                .functions()
                .iter()
                .filter(|candidate| candidate.flags.operator && candidate.kind == kind)
                .cloned()
                .collect::<Vec<_>>();
            let callables =
                crate::libraries::Callables::Functions(crate::libraries::FunctionSet { overloads });
            match resolver.select_receiver_function_with_applied_receiver_tracking(
                delegate_ty,
                name,
                &call_args,
                &[],
                &callables,
            ) {
                crate::symbol_resolver::CandidateSelection::Selected((
                    selected,
                    _,
                    ret,
                    applied_receiver,
                )) => Some((selected, ret, applied_receiver)),
                crate::symbol_resolver::CandidateSelection::None
                | crate::symbol_resolver::CandidateSelection::Ambiguous => None,
            }
        };
        let selected = if let Some(selected) = select_kind(crate::libraries::FnKind::Member) {
            selected
        } else {
            let syntax = vec![delegate; args.len()];
            let member_extension = member_extension_function_with(
                &self.fed_source(),
                self,
                &self.implicit_receivers(scope),
                self.file.explicit_context_arguments,
                &|parameters| self.select_context_arguments_with_types(scope, parameters),
                &|_| false,
                &|_, _, _| None,
                &|params, call_sig, slots| {
                    let mapped = call_argument_parameter_indices(
                        slots.args.len(),
                        params.len(),
                        slots.arg_names,
                        slots.trailing_lambda,
                        call_sig,
                    )?;
                    let mut score = 0;
                    for (source, parameter) in mapped.into_iter().enumerate() {
                        let expected = *params.get(parameter)?;
                        let actual = *args.get(source)?;
                        if !call_sig.parameter_admits(parameter, expected, actual) {
                            return None;
                        }
                        score += self.member_argument_score(expected, actual)?;
                    }
                    Some(CallCandidateScore {
                        rank: (score, std::cmp::Reverse(0), !call_sig.vararg),
                        sam_signatures: vec![None; slots.args.len()],
                    })
                },
                MemberExtensionFunctionCall {
                    extension_receiver: delegate_ty,
                    expected_result,
                    name,
                    args: &syntax,
                    arg_tys: args,
                    arg_names: None,
                    explicit_type_args: &[],
                    trailing_lambda: false,
                },
                MemberExtensionSelection::Operators,
            )
            .ok()
            .flatten();
            if let Some(selected) = member_extension {
                if !self.member_accessible(selected.visibility, selected.owner) {
                    return None;
                }
                let interface = resolver
                    .classifier(selected.owner)
                    .is_some_and(|shape| shape.is_interface());
                return Some(DelegateGetValueTarget::MemberExtension {
                    stable_declaration: selected.stable_declaration,
                    external_identity: selected.external_identity,
                    owner: selected.owner,
                    name: selected.physical_name,
                    extension_receiver: selected.extension_receiver,
                    dispatch_receiver: self.implicit_receiver_selection(selected.dispatch_receiver),
                    context_count: selected.context_args.len(),
                    params: selected.params,
                    ret: selected.ret,
                    physical_params: selected.physical_params,
                    physical_ret: selected.physical_ret,
                    inline: selected.inline,
                    inline_body_plan: selected.inline_body_plan,
                    suspend: selected.suspend,
                    declared_ret: selected.declared_ret,
                    interface,
                });
            }
            select_kind(crate::libraries::FnKind::Extension)?
        };
        let (selected, ret, applied_receiver) = selected;
        let (declared_receiver, declared_ret) = selected
            .stable_declaration
            .and_then(|declaration| self.stable_member_declared_shape(declaration))
            .unwrap_or_else(|| {
                (
                    selected.semantic_receiver().unwrap_or(delegate_ty),
                    selected.semantic_signature().ret,
                )
            });
        if selected.is_extension() {
            let stable_declaration = selected.stable_declaration;
            return resolver
                .build_extension_callable(name, delegate_ty, args, &[], &selected)
                .map(Box::new)
                .map(|callable| DelegateGetValueTarget::Extension {
                    callable,
                    stable_declaration,
                });
        }
        let internal = delegate_ty.obj_internal()?;
        let resolved = resolver.materialize_member_function(delegate_ty, &call_args, &[], selected);
        let owner = resolved.member.owner.unwrap_or(internal);
        let interface = resolved.member.is_interface()
            || resolver
                .classifier(owner)
                .is_some_and(|classifier| classifier.is_interface());
        Some(DelegateGetValueTarget::Member {
            applied_receiver,
            declared_receiver,
            declared_ret,
            stable_declaration: resolved.member.stable_declaration,
            external_identity: resolved.member.external_identity,
            owner,
            name: resolved
                .member
                .physical_name
                .clone()
                .unwrap_or_else(|| resolved.member.name.clone()),
            params: resolved.member.params,
            ret,
            physical_params: resolved.physical_params,
            physical_ret: resolved.member.physical_ret,
            descriptor: resolved.member.descriptor,
            interface,
        })
    }

    fn stable_member_declared_shape(
        &self,
        declaration: crate::fir::DeclarationId,
    ) -> Option<(Ty, Ty)> {
        let index = self.resolved_index?;
        let owner = index.declaration_anchor(declaration)?.owner?;
        let classifier = index.classifier_header(owner)?.classifier;
        let arguments = index
            .classifier_type_arguments(owner)?
            .iter()
            .map(|parameter| {
                let header = index.type_parameter_header(*parameter)?;
                let name = index.type_parameter_semantic_name(*parameter)?;
                let bound = header
                    .bounds
                    .first()
                    .map(|bound| bound.ty.get())
                    .unwrap_or_else(|| Ty::nullable(Ty::obj("kotlin/Any")));
                Some(Ty::ty_param(name, bound))
            })
            .collect::<Option<Vec<_>>>()?;
        let receiver = Ty::obj_args_name(classifier, &arguments);
        let result = index.signature(declaration)?.result.get();
        Some((receiver, result))
    }
}
