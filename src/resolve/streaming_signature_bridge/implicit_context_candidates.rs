//! Context-parameter projection for signature candidate families.
//!
//! A declaration with satisfied implicit context is projected to its source-visible value
//! parameters. An unavailable context is retained as an applicability outcome for diagnostics;
//! it must not erase the declaration's identity and turn the call into an unresolved name.

use super::*;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct SignatureImplicitReceiver {
    pub(super) ty: Ty,
    pub(super) context_parameter: bool,
}

impl SignatureImplicitReceiver {
    pub(super) fn ordinary(ty: Ty) -> Self {
        Self {
            ty,
            context_parameter: false,
        }
    }

    pub(super) fn context_parameter(ty: Ty) -> Self {
        Self {
            ty,
            context_parameter: true,
        }
    }
}

impl ProductionSignatureSemantics<'_> {
    /// Bind every context parameter that the current receiver tower can supply, even when another
    /// context parameter is missing. An unavailable context rejects the call, but its available
    /// siblings remain valid inference evidence for the rejected call's demanded result.
    pub(super) fn implicit_context_candidate_bindings(
        &self,
        scope: crate::fir::SignatureScope,
        candidate: &crate::libraries::FunctionInfo,
        type_arguments: &[Ty],
        expected: Option<Ty>,
    ) -> crate::symbol_resolver::GSigBinds {
        let module = crate::module_symbols::ModuleSymbols::for_file(self.table, scope.source.raw());
        let source = crate::symbol_source::CompositeSource::new(vec![
            &module as &dyn crate::symbol_source::SymbolSource,
            &*self.table.libraries as &dyn crate::symbol_source::SymbolSource,
        ]);
        let receivers = self.implicit_receivers(scope);
        let signature = candidate.semantic_signature();
        let context_count = candidate.context_count.min(signature.params.len());
        let (bindings, fixed_formals) =
            Self::context_call_constraints(&signature, type_arguments, expected);
        Self::available_context_bindings(
            &source,
            &receivers,
            &signature.params[..context_count],
            bindings,
            &fixed_formals,
        )
        .0
    }

    /// Bind implicit context parameters from the compact receiver tower, then expose only the
    /// declaration's source-visible value parameters to ordinary argument mapping and overload
    /// selection. Context values are inference evidence, but never positional call arguments.
    pub(super) fn implicit_context_candidates(
        &self,
        scope: crate::fir::SignatureScope,
        candidates: impl IntoIterator<Item = crate::libraries::FunctionInfo>,
    ) -> Vec<crate::libraries::FunctionInfo> {
        self.partition_implicit_context_candidates(scope, candidates)
            .0
    }

    /// Project candidates whose context parameters can be supplied and retain the exact
    /// declarations whose context is unavailable.
    pub(super) fn partition_implicit_context_candidates(
        &self,
        scope: crate::fir::SignatureScope,
        candidates: impl IntoIterator<Item = crate::libraries::FunctionInfo>,
    ) -> (
        Vec<crate::libraries::FunctionInfo>,
        Vec<crate::libraries::FunctionInfo>,
    ) {
        self.partition_implicit_context_candidates_with_constraints(scope, candidates, &[], None)
    }

    pub(super) fn partition_implicit_context_candidates_with_constraints(
        &self,
        scope: crate::fir::SignatureScope,
        candidates: impl IntoIterator<Item = crate::libraries::FunctionInfo>,
        type_arguments: &[Ty],
        expected: Option<Ty>,
    ) -> (
        Vec<crate::libraries::FunctionInfo>,
        Vec<crate::libraries::FunctionInfo>,
    ) {
        let module = crate::module_symbols::ModuleSymbols::for_file(self.table, scope.source.raw());
        let source = crate::symbol_source::CompositeSource::new(vec![
            &module as &dyn crate::symbol_source::SymbolSource,
            &*self.table.libraries as &dyn crate::symbol_source::SymbolSource,
        ]);
        let receivers = self.implicit_receivers(scope);
        let mut available = Vec::new();
        let mut unavailable = Vec::new();
        for candidate in candidates {
            let signature = candidate.semantic_signature();
            let (bindings, fixed_formals) =
                Self::context_call_constraints(&signature, type_arguments, expected);
            match Self::project_implicit_context_candidate(
                &source,
                &receivers,
                candidate,
                bindings,
                &fixed_formals,
            ) {
                Ok((candidate, _)) => available.push(candidate),
                Err(candidate) => unavailable.push(*candidate),
            }
        }
        (available, unavailable)
    }

    fn project_implicit_context_candidate(
        source: &dyn crate::symbol_source::SymbolSource,
        receivers: &[Ty],
        mut candidate: crate::libraries::FunctionInfo,
        bindings: crate::symbol_resolver::GSigBinds,
        fixed_formals: &[String],
    ) -> Result<
        (
            crate::libraries::FunctionInfo,
            crate::symbol_resolver::GSigBinds,
        ),
        Box<crate::libraries::FunctionInfo>,
    > {
        let mut signature = candidate.semantic_signature().into_owned();
        let context_count = candidate.context_count.min(signature.params.len());
        if context_count == 0 {
            return Ok((candidate, bindings));
        }
        let context_parameters = &signature.params[..context_count];
        let (bindings, complete) = Self::available_context_bindings(
            source,
            receivers,
            context_parameters,
            bindings,
            fixed_formals,
        );
        crate::trace_compiler!(
            "signature",
            "context candidate name={} receivers={receivers:?} parameters={context_parameters:?} complete={complete} bindings={bindings:?}",
            candidate.callable.name,
        );
        if !complete {
            return Err(Box::new(candidate));
        }
        signature.params = signature.params[context_count..]
            .iter()
            .map(|parameter| crate::symbol_resolver::ty_subst_keep_unbound(*parameter, &bindings))
            .collect();
        signature.ret = crate::symbol_resolver::ty_subst_keep_unbound(signature.ret, &bindings);
        let visible = (context_count..candidate.call_sig.param_names.len()).collect::<Vec<_>>();
        candidate.call_sig = candidate.call_sig.select_parameters(&visible);
        candidate.generic_sig = Some(signature);
        candidate.context_count = 0;
        crate::trace_compiler!(
            "signature",
            "context-projected candidate name={} receivers={receivers:?} params={:?} formals={:?} type_bindings={bindings:?}",
            candidate.callable.name,
            candidate.semantic_params(),
            candidate
                .generic_sig
                .as_ref()
                .map(|signature| signature.formals.as_slice())
                .unwrap_or_default(),
        );
        Ok((candidate, bindings))
    }

    fn context_call_constraints(
        signature: &crate::libraries::GenericSig,
        type_arguments: &[Ty],
        expected: Option<Ty>,
    ) -> (crate::symbol_resolver::GSigBinds, Vec<String>) {
        let mut bindings = crate::symbol_resolver::seeded_gsig_binds(signature, type_arguments);
        let mut fixed_formals = signature
            .formals
            .iter()
            .take(type_arguments.len())
            .cloned()
            .collect::<Vec<_>>();
        if type_arguments.is_empty() {
            if let Some(expected) = expected {
                let mut inferred = crate::symbol_resolver::GSigBinds::default();
                crate::symbol_resolver::unify_ty(signature.ret, expected, &mut inferred);
                for (formal, ty) in inferred {
                    if signature.formals.iter().any(|declared| *declared == formal)
                        && !ty.mentions_pending()
                        && ty != Ty::Error
                    {
                        bindings.entry(formal).or_insert(ty);
                        fixed_formals.push(formal.to_string());
                    }
                }
            }
        }
        (bindings, fixed_formals)
    }

    fn available_context_bindings(
        source: &dyn crate::symbol_source::SymbolSource,
        receivers: &[Ty],
        parameters: &[Ty],
        mut bindings: crate::symbol_resolver::GSigBinds,
        fixed_formals: &[String],
    ) -> (crate::symbol_resolver::GSigBinds, bool) {
        let oracle = crate::symbol_resolver::SourceOracle(source);
        let mut complete = true;
        for &parameter in parameters {
            let expected = crate::symbol_resolver::ty_subst_keep_unbound(parameter, &bindings);
            let actual = super::super::context_argument_types(
                receivers,
                std::slice::from_ref(&expected),
                &oracle,
            )
            .and_then(|actual| actual.into_iter().next());
            match actual {
                Some(actual) => {
                    let fixed_bindings = fixed_formals
                        .iter()
                        .filter_map(|formal| {
                            bindings
                                .get(formal.as_str())
                                .copied()
                                .map(|ty| (formal.clone(), ty))
                        })
                        .collect::<Vec<_>>();
                    crate::symbol_resolver::unify_inferred_ty(parameter, actual, &mut bindings);
                    for (formal, ty) in fixed_bindings {
                        bindings.insert(crate::types::TyParamName::new(&formal), ty);
                    }
                }
                None => complete = false,
            }
        }
        (bindings, complete)
    }
}
