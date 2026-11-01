//! Calls through a classifier qualifier (`Classifier.name(...)`).
//!
//! The shared resolver collects the classifier's callable family; these operations map the
//! signature arguments onto it, select one member, and consume its demanded source signature.

use super::*;

/// The member a classifier-qualified call names: `classifier.spelling(...)`.
#[derive(Clone, Copy)]
pub(super) struct ClassifierMember<'a> {
    pub(super) classifier: crate::types::TypeName,
    pub(super) spelling: &'a str,
}

impl ProductionSignatureSemantics<'_> {
    /// Select the ordinary callable family denoted by `Classifier.name(...)`. The shared resolver
    /// owns whether the declaration is a Java static, Kotlin companion instance/static, or an
    /// implicit enum callable; compact signature solving only consumes the selected semantic member
    /// and, for source declarations, demands its stable inferred result.
    pub(super) fn classifier_call_argument_expectations(
        &self,
        scope: crate::fir::SignatureScope,
        member: ClassifierMember<'_>,
        arguments: &[crate::fir::SigCallArgumentProbe<'_>],
        type_arguments: &[crate::fir::ResolvedTy],
        trailing_lambda: bool,
        refresh_typed_postponed: bool,
    ) -> Result<Box<[Option<crate::fir::ResolvedTy>]>, crate::fir::DiagnosticId> {
        let ClassifierMember {
            classifier,
            spelling,
        } = member;
        let resolved_type_arguments = type_arguments
            .iter()
            .map(|argument| argument.get())
            .collect::<Vec<_>>();
        let (parameters, call_sig) = self.with_resolver(scope, |resolver| {
            let (receiver, candidates) =
                resolver.classifier_call_candidates(classifier, spelling)?;
            let (kinds, _) = Self::probe_call_arguments(&candidates, arguments, trailing_lambda)?;
            crate::trace_compiler!(
                "signature",
                "classifier call expectation candidates classifier={classifier:?} spelling={spelling} kinds={kinds:?} candidates={:?}",
                candidates
                    .iter()
                    .map(|candidate| (
                        candidate.callable.owner,
                        candidate.callable.descriptor.as_str(),
                        candidate.semantic_params(),
                        candidate.call_sig.required,
                    ))
                    .collect::<Vec<_>>(),
            );
            let callables = crate::libraries::Callables::from_parts(
                crate::libraries::FunctionSet {
                    overloads: candidates.clone(),
                },
                crate::libraries::PropertySet::default(),
            );
            let selected = match resolver.select_receiver_function_with_params_tracking(
                receiver,
                spelling,
                &kinds,
                &resolved_type_arguments,
                &callables,
                None,
            ) {
                crate::symbol_resolver::CandidateSelection::Selected((selected, _, _)) => {
                    selected
                }
                crate::symbol_resolver::CandidateSelection::None
                | crate::symbol_resolver::CandidateSelection::Ambiguous => {
                    Self::uniquely_mapped_candidate(&candidates, arguments, trailing_lambda)?
                }
            };
            let parameters = crate::symbol_resolver::specialized_function_params(
                &selected,
                &kinds,
                &resolved_type_arguments,
            );
            let parameters = parameters
                .get(selected.context_count.min(parameters.len())..)
                .unwrap_or_default()
                .iter()
                .copied();
            Some((
                Self::functional_parameter_shapes(resolver, &selected, parameters),
                selected.call_sig,
            ))
        })?;
        Self::postponed_call_expectations(
            arguments,
            &parameters,
            &call_sig,
            trailing_lambda,
            refresh_typed_postponed,
        )
        .ok_or_else(Self::failure)
    }

    pub(super) fn select_classifier_call(
        &self,
        scope: crate::fir::SignatureScope,
        member: ClassifierMember<'_>,
        arguments: &[crate::fir::ResolvedSigCallArgument<'_>],
        type_arguments: &[crate::fir::ResolvedTy],
        trailing_lambda: bool,
        demand: &mut dyn FnMut(
            crate::fir::DeclarationId,
        )
            -> Result<crate::fir::ResolvedSignature, crate::fir::DiagnosticId>,
    ) -> Result<Option<crate::fir::ResolvedTy>, crate::fir::DiagnosticId> {
        let ClassifierMember {
            classifier,
            spelling,
        } = member;
        let resolved_type_arguments = type_arguments
            .iter()
            .map(|argument| argument.get())
            .collect::<Vec<_>>();
        let selected = self.with_resolver(scope, |resolver| {
            let (receiver, candidates) =
                resolver.classifier_call_candidates(classifier, spelling)?;
            let (arguments, argument_types) =
                Self::mapped_call_arguments(&candidates, arguments, trailing_lambda)?;
            let member = resolver.select_classifier_callable_from_candidates(
                receiver,
                spelling,
                &arguments,
                &resolved_type_arguments,
                candidates,
            )?;
            Some((member, argument_types))
        });
        let Ok((member, argument_types)) = selected else {
            return Ok(None);
        };
        let receiver = member.receiver;
        if let Some(declaration) = member.member.stable_declaration {
            if self
                .headers
                .stub(declaration)
                .is_some_and(|stub| stub.signature_inference.is_some())
            {
                let signature = demand(declaration)?;
                return self
                    .apply_demanded_member(
                        receiver,
                        &member.member,
                        &signature,
                        &argument_types,
                        &resolved_type_arguments,
                    )
                    .map(Some);
            }
        }
        if let Some(signature) =
            self.demanded_member_signature(member.member.stable_declaration, demand)?
        {
            return self
                .apply_demanded_member(
                    receiver,
                    &member.member,
                    &signature,
                    &argument_types,
                    &resolved_type_arguments,
                )
                .map(Some);
        }
        crate::fir::ResolvedTy::new(member.ret)
            .map(Some)
            .map_err(|_| Self::failure())
    }
}
