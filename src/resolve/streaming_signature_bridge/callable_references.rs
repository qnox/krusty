//! Callable-reference selection that is independent of parser-owned expressions.
//!
//! Compact signature evaluation supplies an expected function shape. This module resolves source
//! type-alias constructor references against the same constructor/SAM declarations and callable
//! adaptation plan used by checked bodies, then returns only the contextual function type.

use super::*;

impl ProductionSignatureSemantics<'_> {
    pub(super) fn applied_source_alias_expansion(
        &self,
        scope: crate::fir::SignatureScope,
        spelling: &str,
        explicit_type_arguments: &[Ty],
    ) -> Option<(Vec<String>, Ty)> {
        let (_, formals, expansion) = self.signature_source_alias_expansion(scope, spelling)?;
        if !explicit_type_arguments.is_empty() && explicit_type_arguments.len() != formals.len() {
            return None;
        }
        let bindings = formals
            .iter()
            .cloned()
            .zip(explicit_type_arguments.iter().copied())
            .collect::<crate::symbol_resolver::GSigBinds>();
        Some((
            formals,
            crate::symbol_resolver::ty_subst_keep_unbound(expansion, &bindings),
        ))
    }

    pub(super) fn apply_source_alias_constructor_result(
        &self,
        scope: crate::fir::SignatureScope,
        formals: &[String],
        expansion: Ty,
        underlying_result: Ty,
        argument: Option<Ty>,
        expected: Option<Ty>,
    ) -> Option<crate::fir::ResolvedTy> {
        let module = crate::module_symbols::ModuleSymbols::for_file(self.table, scope.source.raw());
        let source = crate::symbol_source::CompositeSource::new(vec![
            &module as &dyn crate::symbol_source::SymbolSource,
            &*self.table.libraries as &dyn crate::symbol_source::SymbolSource,
        ]);
        let mut bindings = crate::symbol_resolver::GSigBinds::new();
        crate::symbol_resolver::unify_ty(expansion, underlying_result, &mut bindings);
        if let (Some(argument), Some(sam)) = (
            argument,
            crate::symbol_resolver::semantic_sam_signature(&source, expansion),
        ) {
            let parameter = Ty::fun_with_shape(
                sam.params,
                sam.ret,
                sam.context_count,
                sam.has_receiver,
                sam.suspend,
            );
            crate::symbol_resolver::unify_ty(parameter, argument, &mut bindings);
        }
        if let Some(expected) = expected.filter(|expected| !expected.is_ty_param()) {
            crate::symbol_resolver::unify_ty(expansion, expected, &mut bindings);
        }
        if formals.iter().any(|formal| {
            super::super::ty_mentions_param(expansion, std::slice::from_ref(formal))
                && !bindings.contains_key(formal)
        }) {
            return None;
        }
        crate::fir::ResolvedTy::new(crate::symbol_resolver::ty_subst_keep_unbound(
            expansion, &bindings,
        ))
        .ok()
    }

    pub(super) fn classifier_constructor_reference(
        &self,
        scope: crate::fir::SignatureScope,
        spelling: &str,
        expected: crate::fir::ResolvedTy,
    ) -> Option<crate::fir::ResolvedTy> {
        let Ty::Fun(expected_function) = expected.get().non_null() else {
            return None;
        };
        let alias = self.applied_source_alias_expansion(scope, spelling, &[]);
        crate::trace_compiler!(
            "signature",
            "classifier constructor reference spelling={spelling} alias={alias:?} expected={:?}",
            expected.get(),
        );
        let (alias_formals, expansion) = match alias {
            Some(alias) => alias,
            None => {
                let classifier = self
                    .with_resolver(scope, |resolver| {
                        match resolver.classifier_in_scope(spelling) {
                            crate::symbol_resolver::CandidateSelection::Selected(classifier) => {
                                Some(classifier)
                            }
                            crate::symbol_resolver::CandidateSelection::Ambiguous
                            | crate::symbol_resolver::CandidateSelection::None => None,
                        }
                    })
                    .ok()?;
                let declaration = self.table.class_by_type_name(classifier);
                let formals = declaration
                    .map(|declaration| declaration.type_params.clone())
                    .unwrap_or_default();
                let arguments = declaration
                    .into_iter()
                    .flat_map(|declaration| {
                        declaration
                            .type_params
                            .iter()
                            .zip(&declaration.type_param_bounds)
                            .map(|(name, bound)| Ty::ty_param(name, *bound))
                    })
                    .collect::<Vec<_>>();
                (formals, Ty::obj_args_name(classifier, &arguments))
            }
        };
        let classifier = expansion.obj_internal()?;
        let module = crate::module_symbols::ModuleSymbols::for_file(self.table, scope.source.raw());
        let source = crate::symbol_source::CompositeSource::new(vec![
            &module as &dyn crate::symbol_source::SymbolSource,
            &*self.table.libraries as &dyn crate::symbol_source::SymbolSource,
        ]);
        let oracle = crate::symbol_resolver::SourceOracle(&source);
        let context = crate::assignable::TyCtx::new();
        let assignable =
            |actual, target| crate::assignable::is_assignable(&context, &oracle, actual, target);
        let mut alias_bindings = crate::symbol_resolver::GSigBinds::new();

        let sam = crate::symbol_resolver::semantic_sam_signature(&source, expansion);
        crate::trace_compiler!(
            "signature",
            "classifier constructor reference spelling={spelling} classifier={:?} sam={sam:?}",
            crate::symbol_source::SymbolSource::classifier(&source, classifier).map(|classifier| (
                classifier.sam_eligible,
                classifier.type_params.clone(),
                classifier.members.len(),
            )),
        );
        let (mut parameters, call_sig) = if let Some(sam) = sam {
            let [actual] = expected_function.params.as_slice() else {
                return None;
            };
            let declared = Ty::fun_with_shape(
                sam.params,
                sam.ret,
                sam.context_count,
                sam.has_receiver,
                sam.suspend,
            );
            crate::symbol_resolver::unify_ty(declared, *actual, &mut alias_bindings);
            (vec![declared], crate::libraries::CallSig::default())
        } else {
            let arguments = expected_function
                .params
                .iter()
                .copied()
                .map(crate::symbol_resolver::CallArgKind::Typed)
                .collect::<Vec<_>>();
            let selected = self
                .with_resolver(scope, |resolver| {
                    resolver.select_constructor_declaration_with_type_arguments(
                        classifier,
                        &arguments,
                        expansion.type_args(),
                    )
                })
                .ok()?;
            let class = self.table.class_by_type_name(classifier)?;
            let class_bindings = class
                .type_params
                .iter()
                .cloned()
                .zip(expansion.type_args().iter().copied())
                .collect::<crate::symbol_resolver::GSigBinds>();
            let declared = selected
                .generic_sig
                .as_ref()
                .map_or(selected.params.as_slice(), |signature| {
                    signature.params.as_slice()
                })
                .iter()
                .map(|parameter| {
                    crate::symbol_resolver::ty_subst_keep_unbound(*parameter, &class_bindings)
                })
                .collect::<Vec<_>>();
            (declared, selected.call_sig.clone())
        };

        // The expected callable inputs are inference evidence before adaptation is judged. This is
        // the same declared-to-actual unification used by ordinary generic callable references; it
        // specializes alias-owned variables so the shared parameter plan compares concrete shapes.
        for (&declared, &actual) in parameters.iter().zip(&expected_function.params) {
            crate::symbol_resolver::unify_ty(declared, actual, &mut alias_bindings);
        }
        for parameter in &mut parameters {
            *parameter = crate::symbol_resolver::ty_subst_keep_unbound(*parameter, &alias_bindings);
        }

        let plan = super::super::callable_reference_selection::parameter_plan(
            &parameters,
            &call_sig,
            &expected_function.params,
            assignable,
        )?;
        for (parameter, argument) in plan.iter().enumerate() {
            let declared = *parameters.get(parameter)?;
            match argument {
                super::super::callable_reference_selection::AdaptedRefArgument::Value(value) => {
                    crate::symbol_resolver::unify_ty(
                        declared,
                        *expected_function.params.get(*value)?,
                        &mut alias_bindings,
                    );
                }
                super::super::callable_reference_selection::AdaptedRefArgument::Vararg {
                    values,
                    whole_array,
                } => {
                    let declared = if *whole_array {
                        declared
                    } else {
                        declared.array_read_elem().unwrap_or(declared)
                    };
                    for value in values {
                        crate::symbol_resolver::unify_ty(
                            declared,
                            *expected_function.params.get(*value)?,
                            &mut alias_bindings,
                        );
                    }
                }
                super::super::callable_reference_selection::AdaptedRefArgument::Default => {}
            }
        }
        if !expected_function.ret.is_ty_param() {
            crate::symbol_resolver::unify_ty(expansion, expected_function.ret, &mut alias_bindings);
        }
        crate::trace_compiler!(
            "signature",
            "classifier constructor reference spelling={spelling} parameters={parameters:?} plan={plan:?} bindings={alias_bindings:?}",
        );
        if alias_formals
            .iter()
            .any(|formal| !alias_bindings.contains_key(formal))
        {
            return None;
        }
        let result = crate::symbol_resolver::ty_subst_keep_unbound(expansion, &alias_bindings);
        let contextual = super::super::callable_reference_selection::realize_expected_shape(
            expected_function,
            &parameters,
            result,
            &plan,
        );
        crate::fir::ResolvedTy::new(contextual).ok()
    }
}
