//! Selection of the `invoke` convention on a callee value. No applicable `invoke` is a typed
//! outcome, not a failure, so a bare call's tower can move on to its later levels.

use super::super::classifier_associated::Demand;
use super::*;

impl ProductionSignatureSemantics<'_> {
    pub(super) fn select_value_invoke(
        &self,
        scope: crate::fir::SignatureScope,
        origin: crate::fir::OriginId,
        callee: crate::fir::ResolvedTy,
        arguments: &[crate::fir::ResolvedSigCallArgument<'_>],
        demand: &mut Demand<'_>,
    ) -> Result<crate::fir::InvokeSelection, crate::fir::DiagnosticId> {
        crate::trace_compiler!(
            "signature",
            "select invoke callee={:?} arguments={:?}",
            callee.get(),
            arguments
                .iter()
                .map(|argument| argument.ty.get())
                .collect::<Vec<_>>(),
        );
        let callable = crate::symbol_resolver::classifier_callable_signature(
            &*self.table.libraries,
            callee.get().non_null(),
        );
        if let Some(TyKind::Fun(signature)) = callable.map(|ty| ty.kind()) {
            let implicit_prefix = signature
                .context_count
                .saturating_add(usize::from(signature.has_receiver))
                .min(signature.params.len());
            let accepts_explicit_shape = signature.params.len() == arguments.len();
            let accepts_implicit_shape =
                signature.params.len() - implicit_prefix == arguments.len();
            if (!accepts_explicit_shape && !accepts_implicit_shape)
                || arguments
                    .iter()
                    .any(|argument| argument.name.is_some() || argument.spread)
            {
                return Ok(crate::fir::InvokeSelection::Inapplicable(Self::failure()));
            }
            self.record_function_value_invoke_constraints(
                scope,
                signature,
                implicit_prefix,
                accepts_explicit_shape,
                arguments,
            );
            return Self::selected_invoke(signature.ret);
        }

        let selected = self.with_resolver(scope, |resolver| {
            let (mut functions, properties) = resolver
                .receiver_callables(callee.get(), "invoke")
                .into_parts();
            functions
                .overloads
                .retain(|candidate| candidate.flags.operator);
            functions.overloads = self.implicit_context_candidates(scope, functions.overloads);
            let callables = crate::libraries::Callables::from_parts(functions, properties);
            let (argument_kinds, argument_types) =
                Self::mapped_call_arguments(callables.functions(), arguments, false)?;
            let crate::symbol_resolver::CandidateSelection::Selected((
                selected,
                parameters,
                result,
            )) = resolver.select_receiver_function_with_params_tracking(
                callee.get(),
                "invoke",
                crate::symbol_resolver::FamilyArguments::Shared(&argument_kinds),
                &[],
                &callables,
                None,
            )
            else {
                return None;
            };
            Some((selected, parameters, result, argument_types))
        });
        if let Ok((selected, parameters, result, argument_types)) = selected {
            if selected.kind == crate::libraries::FnKind::Member {
                let mut member = selected.member_with_return(result);
                member.params = parameters;
                if let Some(signature) =
                    self.demanded_member_signature(member.stable_declaration, demand)?
                {
                    return self
                        .apply_demanded_member(
                            callee.get(),
                            &member,
                            &signature,
                            &argument_types,
                            &[],
                        )
                        .map(crate::fir::InvokeSelection::Selected);
                }
            }
            if let Some(source) = selected.source_key {
                if let Some(signature) =
                    self.demanded_source_signature(None, selected.stable_declaration, demand)?
                {
                    return self
                        .apply_demanded_source_callable(
                            source,
                            &signature,
                            super::super::demanded_source_call::DemandedSourceInputs {
                                receiver: Some(callee.get()),
                                arguments: &argument_types,
                                argument_kinds: None,
                                explicit_type_arguments: &[],
                                expected: None,
                            },
                        )
                        .map(crate::fir::InvokeSelection::Selected);
                }
            }
            return Self::selected_invoke(result);
        }

        let argument_names = arguments
            .iter()
            .map(|argument| argument.name.map(str::to_owned))
            .collect::<Vec<_>>();
        let argument_names = argument_names
            .iter()
            .any(Option::is_some)
            .then_some(argument_names.as_slice());
        let argument_kinds = arguments
            .iter()
            .map(Self::call_argument_kind)
            .collect::<Vec<_>>();
        for dispatch in self.signature_dispatch_receivers(scope) {
            let selected = self.signature_member_extension_call(
                scope,
                super::super::lookups::MemberExtensionReceivers {
                    extension: callee.get(),
                    dispatch,
                },
                "invoke",
                super::super::lookups::SignatureMemberExtensionArguments {
                    arguments: &argument_kinds,
                    names: argument_names,
                    ..Default::default()
                },
                super::super::super::CallResultConstraint::None,
                super::super::super::MemberExtensionSelection::Operators,
            );
            let (result, declaration) = match selected {
                super::super::lookups::SignatureMemberExtensionCallSelection::Selected {
                    result,
                    declaration,
                } => (result, declaration),
                super::super::lookups::SignatureMemberExtensionCallSelection::None(_) => continue,
                super::super::lookups::SignatureMemberExtensionCallSelection::Ambiguous(_) => {
                    return Err(self.record_ambiguous_member(scope.owner, origin, "invoke"));
                }
            };
            if let Some(declaration) = declaration {
                if self
                    .headers
                    .stub(declaration)
                    .is_some_and(|stub| stub.signature_inference.is_some())
                {
                    return demand(declaration)
                        .map(|signature| crate::fir::InvokeSelection::Selected(signature.result));
                }
            }
            return Self::selected_invoke(result);
        }
        Ok(crate::fir::InvokeSelection::Inapplicable(Self::failure()))
    }

    /// The result of the `invoke` selected on `callee`; an inapplicable callee is a failure here.
    pub(super) fn invoke_result(
        &self,
        scope: crate::fir::SignatureScope,
        origin: crate::fir::OriginId,
        callee: crate::fir::ResolvedTy,
        arguments: &[crate::fir::ResolvedSigCallArgument<'_>],
        demand: &mut Demand<'_>,
    ) -> Result<crate::fir::ResolvedTy, crate::fir::DiagnosticId> {
        match self.select_value_invoke(scope, origin, callee, arguments, demand)? {
            crate::fir::InvokeSelection::Selected(result) => Ok(result),
            crate::fir::InvokeSelection::Inapplicable(diagnostic) => Err(diagnostic),
        }
    }

    /// The result of an `invoke` the callee selected.
    fn selected_invoke(
        result: Ty,
    ) -> Result<crate::fir::InvokeSelection, crate::fir::DiagnosticId> {
        crate::fir::ResolvedTy::new(result)
            .map(crate::fir::InvokeSelection::Selected)
            .map_err(|_| Self::failure())
    }
}
