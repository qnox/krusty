//! Package-qualified callable selection for compact signature evaluation.
//!
//! Qualification changes the resolver's package scope, but not overload selection, argument
//! mapping, generic inference, or postponed-argument shaping. Those decisions stay in the shared
//! resolver and call-signature operations used by ordinary frontend checking.

use super::*;

impl ProductionSignatureSemantics<'_> {
    fn qualified_package(
        &self,
        qualifier: &str,
        module: &crate::module_symbols::ModuleSymbols<'_>,
    ) -> Result<crate::types::TypeName, crate::fir::DiagnosticId> {
        let source = crate::symbol_source::CompositeSource::new(vec![
            module as &dyn crate::symbol_source::SymbolSource,
            self.table.libraries.as_ref() as &dyn crate::symbol_source::SymbolSource,
        ]);
        let super::super::ResolvedQualifier::Package(package) =
            super::super::qualifier_path(qualifier, &source, None).map_err(|_| Self::failure())?
        else {
            return Err(Self::failure());
        };
        Ok(package)
    }

    pub(super) fn select_qualified_package_call(
        &self,
        scope: crate::fir::SignatureScope,
        spelling: &str,
        arguments: &[crate::fir::ResolvedSigCallArgument<'_>],
        type_arguments: &[crate::fir::ResolvedTy],
        trailing_lambda: bool,
        demand: &mut dyn FnMut(
            crate::fir::DeclarationId,
        )
            -> Result<crate::fir::ResolvedSignature, crate::fir::DiagnosticId>,
    ) -> Result<crate::fir::ResolvedTy, crate::fir::DiagnosticId> {
        let (qualifier, name) = spelling.rsplit_once('.').ok_or_else(Self::failure)?;
        let module = crate::module_symbols::ModuleSymbols::for_file(self.table, scope.source.raw());
        let package = self.qualified_package(qualifier, &module)?;
        let packages = [package];
        let resolver = crate::symbol_resolver::SymbolResolver::new_scoped_with_module(
            self.table.libraries.as_ref(),
            &module,
            &packages,
        );
        let candidates = self
            .implicit_context_candidates(scope, resolver.top_level_candidates(name).into_iter());
        let (selected_arguments, selected_argument_types) =
            Self::mapped_call_arguments(&candidates, arguments, trailing_lambda)
                .ok_or_else(Self::failure)?;
        crate::trace_compiler!(
            "signature",
            "qualified package candidates spelling={spelling} shapes={:?} arguments={selected_arguments:?}",
            candidates
                .iter()
                .map(|candidate| (
                    candidate.semantic_params().into_owned(),
                    candidate.context_count,
                    candidate.call_sig.required,
                    candidate.call_sig.param_defaults.clone(),
                ))
                .collect::<Vec<_>>(),
        );
        let type_arguments = type_arguments
            .iter()
            .map(|argument| argument.get())
            .collect::<Vec<_>>();
        let (selected, callable) = resolver
            .select_top_level_function_candidates(
                name,
                candidates,
                &selected_arguments,
                &type_arguments,
            )
            .ok_or_else(Self::failure)?;
        if let Some(source) = selected.source_key {
            if let Some(signature) =
                self.demanded_source_signature(None, selected.stable_declaration, demand)?
            {
                return self.apply_demanded_source_callable(
                    source,
                    None,
                    &signature,
                    &selected_argument_types,
                    None,
                    &type_arguments,
                    None,
                );
            }
        }
        crate::trace_compiler!(
            "signature",
            "qualified package call selected spelling={spelling} package={:?} result={:?}",
            package,
            callable.ret,
        );
        crate::fir::ResolvedTy::new(callable.ret).map_err(|_| Self::failure())
    }

    pub(super) fn qualified_package_call_argument_expectations(
        &self,
        scope: crate::fir::SignatureScope,
        spelling: &str,
        arguments: &[crate::fir::SigCallArgumentProbe<'_>],
        type_arguments: &[crate::fir::ResolvedTy],
        trailing_lambda: bool,
    ) -> Result<Box<[Option<crate::fir::ResolvedTy>]>, crate::fir::DiagnosticId> {
        let (qualifier, name) = spelling.rsplit_once('.').ok_or_else(Self::failure)?;
        let module = crate::module_symbols::ModuleSymbols::for_file(self.table, scope.source.raw());
        let package = self.qualified_package(qualifier, &module)?;
        let packages = [package];
        let resolver = crate::symbol_resolver::SymbolResolver::new_scoped_with_module(
            self.table.libraries.as_ref(),
            &module,
            &packages,
        );
        let candidates = self
            .implicit_context_candidates(scope, resolver.top_level_candidates(name).into_iter());
        let (kinds, _slots) = Self::probe_call_arguments(&candidates, arguments, trailing_lambda)
            .ok_or_else(Self::failure)?;
        let type_arguments = type_arguments
            .iter()
            .map(|argument| argument.get())
            .collect::<Vec<_>>();
        let selected = resolver
            .select_top_level_function_candidates(name, candidates.clone(), &kinds, &type_arguments)
            .map(|(selected, _)| selected)
            .or_else(|| match candidates.as_slice() {
                [only] => Some(only.clone()),
                [] | [_, _, ..] => None,
            })
            .ok_or_else(Self::failure)?;
        let parameters = Self::functional_parameter_shapes(
            &resolver,
            &selected,
            crate::symbol_resolver::specialized_function_params(&selected, &kinds, &type_arguments),
        );
        Self::postponed_call_expectations(
            arguments,
            &parameters,
            &selected.call_sig,
            trailing_lambda,
        )
        .ok_or_else(Self::failure)
    }
}
