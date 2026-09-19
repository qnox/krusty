//! The delegated-property signature boundary.
//!
//! A property declared `by` an expression takes its type from the convention that expression
//! supplies, so signature solving has to select `provideDelegate`/`getValue` before it can answer
//! what the declaration IS. Everything that selection needs — the `thisRef` the accessors pass,
//! where the diagnostic points, and the refusal when no convention applies — is one responsibility
//! and lives here rather than in the signature facade or the general semantics implementation.

use super::*;

impl ProductionSignatureSemantics<'_> {
    pub(super) fn select_delegate_signature(
        &self,
        scope: crate::fir::SignatureScope,
        delegate: crate::fir::ResolvedTy,
        site: crate::fir::ResolvedSignatureDelegateSite,
        demand: &mut dyn FnMut(
            crate::fir::DeclarationId,
        )
            -> Result<crate::fir::ResolvedSignature, crate::fir::DiagnosticId>,
    ) -> Result<crate::fir::ResolvedTy, crate::fir::DiagnosticId> {
        let (provide_ref, this_ref) = self.delegate_first_arguments(&site);
        // The classifier is resolved through the same source that answers applicability, so this
        // probe and the checked selection agree on which declaration `KProperty` is — and a
        // dependency set that declares none answers `None` here, exactly as a missing convention
        // does.
        let property_reference = match self.with_resolver(scope, |resolver| {
            Some(crate::resolve::delegated_properties::delegate_property_reference_type(resolver))
        }) {
            Ok(Some(property_reference)) => property_reference,
            // Without the classifier the accessors pass there is no convention to select against,
            // which is the same refusal a missing `getValue` is and is reported the same way. The
            // alternative — manufacturing the spelling — selects against a declaration the
            // dependency set does not have.
            Ok(None) => {
                return Err(self.record_delegate_convention_failure(
                    scope,
                    site,
                    delegate.get(),
                    "getValue",
                    this_ref,
                    demand,
                ));
            }
            Err(diagnostic) if diagnostic.raw() == 0 => {
                panic!("a retained delegate signature scope must construct its resolver")
            }
            Err(diagnostic) => return Err(diagnostic),
        };
        let provide_arguments = [provide_ref, property_reference];
        let provided = match self.with_resolver(scope, |resolver| {
            Some(super::super::select_delegate_operator(
                resolver,
                delegate.get(),
                "provideDelegate",
                &provide_arguments,
            ))
        }) {
            Ok(provided) => provided,
            Err(diagnostic) if diagnostic.raw() == 0 => {
                panic!("a retained delegate signature scope must construct its resolver")
            }
            Err(diagnostic) => return Err(diagnostic),
        };
        crate::trace_compiler!(
            "signature",
            "delegate provide receiver={:?} selection={:?}",
            delegate.get(),
            match &provided {
                crate::resolve::delegated_properties::DelegateConventionSelection::Selected(
                    selected,
                    result,
                ) => Some((
                    selected.semantic_receiver(),
                    selected.semantic_params(),
                    *result,
                )),
                crate::resolve::delegated_properties::DelegateConventionSelection::None
                | crate::resolve::delegated_properties::DelegateConventionSelection::Ambiguous(_) =>
                    None,
            },
        );
        let stored = match provided {
            crate::resolve::delegated_properties::DelegateConventionSelection::None => {
                let mut selected = None;
                for dispatch in self.implicit_receivers(scope) {
                    match self.signature_member_extension_call(
                        scope,
                        delegate.get(),
                        dispatch,
                        "provideDelegate",
                        super::lookups::SignatureMemberExtensionArguments {
                            types: &provide_arguments,
                            ..Default::default()
                        },
                        None,
                        super::super::MemberExtensionSelection::DelegateConventions,
                    ) {
                        super::lookups::SignatureMemberExtensionCallSelection::Selected {
                            result,
                            declaration,
                        } => {
                            selected = Some((result, declaration));
                            break;
                        }
                        super::lookups::SignatureMemberExtensionCallSelection::None(_) => continue,
                        super::lookups::SignatureMemberExtensionCallSelection::Ambiguous(_) => {
                            selected = None;
                            break;
                        }
                    }
                }
                match selected {
                    Some((_result, Some(declaration)))
                        if self
                            .headers
                            .stub(declaration)
                            .is_some_and(|stub| stub.signature_inference.is_some()) =>
                    {
                        self.delegate_result_or_invariant(
                            &site,
                            "provideDelegate",
                            demand(declaration).map(|signature| signature.result),
                        )?
                    }
                    Some((result, _)) => {
                        self.resolved_delegate_result(&site, "provideDelegate", result)
                    }
                    None => delegate,
                }
            }
            crate::resolve::delegated_properties::DelegateConventionSelection::Ambiguous(_) => {
                delegate
            }
            crate::resolve::delegated_properties::DelegateConventionSelection::Selected(
                selected,
                result,
            ) => self.delegate_result_or_invariant(
                &site,
                "provideDelegate",
                self.selected_convention_result(
                    delegate.get(),
                    &selected,
                    result,
                    &provide_arguments,
                    demand,
                ),
            )?,
        };
        let get_arguments = [this_ref, property_reference];
        let selected = match self.with_resolver(scope, |resolver| {
            Some(super::super::select_delegate_operator(
                resolver,
                stored.get(),
                "getValue",
                &get_arguments,
            ))
        }) {
            Ok(selected) => selected,
            Err(diagnostic) if diagnostic.raw() == 0 => {
                panic!("a retained delegate signature scope must construct its resolver")
            }
            Err(diagnostic) => return Err(diagnostic),
        };
        crate::trace_compiler!(
            "signature",
            "delegate get receiver={:?} selection={:?}",
            stored.get(),
            match &selected {
                crate::resolve::delegated_properties::DelegateConventionSelection::Selected(
                    selected,
                    result,
                ) => Some((
                    selected.semantic_receiver(),
                    selected.semantic_params(),
                    *result,
                )),
                crate::resolve::delegated_properties::DelegateConventionSelection::None
                | crate::resolve::delegated_properties::DelegateConventionSelection::Ambiguous(_) =>
                    None,
            },
        );
        match selected {
            crate::resolve::delegated_properties::DelegateConventionSelection::Selected(
                selected,
                result,
            ) => self.delegate_result_or_invariant(
                &site,
                "getValue",
                self.selected_convention_result(
                    stored.get(),
                    &selected,
                    result,
                    &get_arguments,
                    demand,
                ),
            ),
            crate::resolve::delegated_properties::DelegateConventionSelection::None => {
                let mut excluded = Vec::new();
                for dispatch in self.implicit_receivers(scope) {
                    let selection = self.signature_member_extension_call(
                        scope,
                        stored.get(),
                        dispatch,
                        "getValue",
                        super::lookups::SignatureMemberExtensionArguments {
                            types: &get_arguments,
                            ..Default::default()
                        },
                        None,
                        super::super::MemberExtensionSelection::DelegateConventions,
                    );
                    let (result, declaration) = match selection {
                        super::lookups::SignatureMemberExtensionCallSelection::Selected {
                            result,
                            declaration,
                        } => (result, declaration),
                        super::lookups::SignatureMemberExtensionCallSelection::None(candidates) => {
                            excluded.extend(candidates);
                            continue;
                        }
                        super::lookups::SignatureMemberExtensionCallSelection::Ambiguous(
                            candidates,
                        ) => {
                            return Err(self
                                .record_member_extension_delegate_convention_ambiguity(
                                    scope,
                                    site,
                                    stored.get(),
                                    "getValue",
                                    this_ref,
                                    &candidates,
                                    demand,
                                ));
                        }
                    };
                    if let Some(declaration) = declaration {
                        if self
                            .headers
                            .stub(declaration)
                            .is_some_and(|stub| stub.signature_inference.is_some())
                        {
                            return self.delegate_result_or_invariant(
                                &site,
                                "getValue",
                                demand(declaration).map(|signature| signature.result),
                            );
                        }
                    }
                    return Ok(self.resolved_delegate_result(&site, "getValue", result));
                }
                if !excluded.is_empty() {
                    return Err(
                        self.record_excluded_member_extension_delegate_convention_failure(
                            scope,
                            site,
                            stored.get(),
                            "getValue",
                            this_ref,
                            &excluded,
                            demand,
                        ),
                    );
                }
                // A property with NO declared type has nothing left to infer its type FROM once
                // `getValue` is missing, so this refusal is where the source mistake is finally
                // known. Declining silently made signature finalization fail with no cause named,
                // which skips body checking for the whole file — the file then reported nothing at
                // all, including its unrelated diagnostics. Name it here, in the same wording the
                // body check uses, so the two collapse wherever both reach the sink.
                Err(self.record_delegate_convention_failure(
                    scope,
                    site,
                    stored.get(),
                    "getValue",
                    this_ref,
                    demand,
                ))
            }
            crate::resolve::delegated_properties::DelegateConventionSelection::Ambiguous(
                candidates,
            ) => Err(self.record_delegate_convention_ambiguity(
                scope,
                site,
                stored.get(),
                "getValue",
                this_ref,
                &candidates,
                demand,
            )),
        }
    }
    /// Record one failed convention selection at the exact `by` keyword retained by extraction.
    fn record_delegate_convention_failure(
        &self,
        scope: crate::fir::SignatureScope,
        site: crate::fir::ResolvedSignatureDelegateSite,
        delegate: Ty,
        name: &str,
        this_ref: Ty,
        demand: &mut dyn FnMut(
            crate::fir::DeclarationId,
        )
            -> Result<crate::fir::ResolvedSignature, crate::fir::DiagnosticId>,
    ) -> crate::fir::DiagnosticId {
        let (source, by_span) = self.delegate_by_location(&site);
        assert_eq!(
            source, scope.source,
            "a delegate convention site must belong to its retained signature scope",
        );
        let convention_site = self.delegate_convention_site(&site, by_span);
        // A candidate's own return may still be undetermined here — this phase is what determines
        // it — and rendering the placeholder produces a second, differently worded message for the
        // same mistake once the body check reports it. Ask the solver for the declaration instead,
        // exactly as a SELECTED convention's result is asked for.
        let message = match self.with_resolver(scope, |resolver| {
            Some(
                crate::resolve::delegated_properties::delegate_convention_message(
                    resolver,
                    convention_site,
                    delegate,
                    name,
                    this_ref,
                    None,
                    None,
                    &mut |candidate| self.determined_candidate_result(candidate, demand),
                ),
            )
        }) {
            Ok(message) => message,
            Err(diagnostic) if diagnostic.raw() == 0 => {
                panic!("a retained delegate signature scope must construct its resolver")
            }
            Err(diagnostic) => return diagnostic,
        };
        match message {
            Ok(Some(message)) => {
                self.record_source_diagnostic_at(site.diagnostic_owner, source, by_span, message)
            }
            Ok(None) => panic!("resolved delegate convention facts must produce a diagnostic"),
            Err(diagnostic) => diagnostic,
        }
    }

    fn record_delegate_convention_ambiguity(
        &self,
        scope: crate::fir::SignatureScope,
        site: crate::fir::ResolvedSignatureDelegateSite,
        delegate: Ty,
        name: &str,
        this_ref: Ty,
        candidates: &[crate::libraries::FunctionInfo],
        demand: &mut dyn FnMut(
            crate::fir::DeclarationId,
        )
            -> Result<crate::fir::ResolvedSignature, crate::fir::DiagnosticId>,
    ) -> crate::fir::DiagnosticId {
        let (source, by_span) = self.delegate_by_location(&site);
        assert_eq!(
            source, scope.source,
            "a delegate convention site must belong to its retained signature scope",
        );
        let convention_site = self.delegate_convention_site(&site, by_span);
        let message = match crate::resolve::delegated_properties::delegate_convention_ambiguity_message_with_functions(
            convention_site,
            delegate,
            name,
            this_ref,
            None,
            None,
            candidates,
            &mut |candidate| self.determined_candidate_result(candidate, demand),
        ) {
            Ok(message) => message,
            Err(diagnostic) if diagnostic.raw() == 0 => {
                panic!("a demanded ambiguous delegate candidate failed without a diagnostic")
            }
            Err(diagnostic) => return diagnostic,
        };
        match message {
            Some(message) => {
                self.record_source_diagnostic_at(site.diagnostic_owner, source, by_span, message)
            }
            None => panic!("ambiguous delegate convention facts must produce a diagnostic"),
        }
    }

    fn record_member_extension_delegate_convention_ambiguity(
        &self,
        scope: crate::fir::SignatureScope,
        site: crate::fir::ResolvedSignatureDelegateSite,
        delegate: Ty,
        name: &str,
        this_ref: Ty,
        candidates: &[super::super::MemberExtensionFunctionCandidate],
        demand: &mut dyn FnMut(
            crate::fir::DeclarationId,
        )
            -> Result<crate::fir::ResolvedSignature, crate::fir::DiagnosticId>,
    ) -> crate::fir::DiagnosticId {
        let (source, by_span) = self.delegate_by_location(&site);
        assert_eq!(
            source, scope.source,
            "a delegate convention site must belong to its retained signature scope",
        );
        let convention_site = self.delegate_convention_site(&site, by_span);
        let candidates = match candidates
            .iter()
            .map(|candidate| {
                let result = if candidate.ret.mentions_pending() {
                    let declaration = candidate.stable_declaration.expect(
                        "an undetermined member-extension delegate candidate retains its declaration",
                    );
                    match demand(declaration) {
                        Ok(signature) => signature.result.get(),
                        Err(diagnostic) if diagnostic.raw() == 0 => {
                            panic!("a demanded member-extension delegate candidate failed without a diagnostic")
                        }
                        Err(diagnostic) => return Err(diagnostic),
                    }
                } else {
                    candidate.ret
                };
                let context_count = candidate.context_count.min(candidate.params.len());
                let (context, value) = candidate.params.split_at(context_count);
                Ok(
                    crate::resolve::delegated_properties::delegate_convention_diagnostic_candidate(
                        name,
                        Some(candidate.extension_receiver),
                        context,
                        value,
                        &candidate.diagnostic_param_names,
                        result,
                    ),
                )
            })
            .collect::<Result<Vec<_>, crate::fir::DiagnosticId>>()
        {
            Ok(candidates) => candidates,
            Err(diagnostic) => return diagnostic,
        };
        let message =
            crate::resolve::delegated_properties::delegate_convention_ambiguity_message_with_candidates(
                convention_site,
                delegate,
                name,
                this_ref,
                None,
                None,
                &candidates,
            )
            .expect("ambiguous member-extension delegate candidates must produce a diagnostic");
        self.record_source_diagnostic_at(site.diagnostic_owner, source, by_span, message)
    }

    fn record_excluded_member_extension_delegate_convention_failure(
        &self,
        scope: crate::fir::SignatureScope,
        site: crate::fir::ResolvedSignatureDelegateSite,
        delegate: Ty,
        name: &str,
        this_ref: Ty,
        candidates: &[super::super::member_extension_selection::MemberExtensionConventionDiagnosticCandidate],
        demand: &mut dyn FnMut(
            crate::fir::DeclarationId,
        )
            -> Result<crate::fir::ResolvedSignature, crate::fir::DiagnosticId>,
    ) -> crate::fir::DiagnosticId {
        let (source, by_span) = self.delegate_by_location(&site);
        assert_eq!(
            source, scope.source,
            "a delegate convention site must belong to its retained signature scope",
        );
        let convention_site = self.delegate_convention_site(&site, by_span);
        let candidates = match candidates
            .iter()
            .map(|candidate| {
                let result = if candidate.ret.mentions_pending() {
                    let declaration = candidate.stable_declaration.expect(
                        "an undetermined excluded delegate candidate retains its declaration",
                    );
                    match demand(declaration) {
                        Ok(signature) => signature.result.get(),
                        Err(diagnostic) if diagnostic.raw() == 0 => {
                            panic!("a demanded excluded delegate candidate failed without a diagnostic")
                        }
                        Err(diagnostic) => return Err(diagnostic),
                    }
                } else {
                    candidate.ret
                };
                let context_count = candidate.context_count.min(candidate.params.len());
                let (context, value) = candidate.params.split_at(context_count);
                Ok(
                    crate::resolve::delegated_properties::delegate_convention_diagnostic_candidate(
                        name,
                        Some(candidate.extension_receiver),
                        context,
                        value,
                        &candidate.parameter_names,
                        result,
                    ),
                )
            })
            .collect::<Result<Vec<_>, crate::fir::DiagnosticId>>()
        {
            Ok(candidates) => candidates,
            Err(diagnostic) => return diagnostic,
        };
        let message =
            crate::resolve::delegated_properties::delegate_convention_message_with_candidates(
                convention_site,
                delegate,
                name,
                this_ref,
                None,
                None,
                &candidates,
            )
            .expect("excluded member-extension delegate candidates must produce a diagnostic");
        self.record_source_diagnostic_at(site.diagnostic_owner, source, by_span, message)
    }

    /// One candidate's result type, resolved when this phase has not determined it yet.
    ///
    /// A declaration whose own return is inferred reads as undetermined in the symbol table until
    /// its signature is solved. The solver answers for it on demand; a refusal carries that
    /// declaration's diagnostic instead of rendering the placeholder or retrying another origin.
    fn determined_candidate_result(
        &self,
        candidate: &crate::libraries::FunctionInfo,
        demand: &mut dyn FnMut(
            crate::fir::DeclarationId,
        )
            -> Result<crate::fir::ResolvedSignature, crate::fir::DiagnosticId>,
    ) -> Result<Ty, crate::fir::DiagnosticId> {
        let recorded = candidate.callable.ret;
        if !recorded.mentions_pending() {
            return Ok(recorded);
        }
        let declaration = candidate
            .stable_declaration
            .expect("an undetermined delegate candidate result must retain its declaration");
        match self.demanded_member_signature(Some(declaration), demand) {
            Ok(Some(signature)) => Ok(signature.result.get()),
            Ok(None) => panic!("a retained delegate candidate declaration must be demandable"),
            Err(diagnostic) if diagnostic.raw() == 0 => {
                panic!("a demanded delegate candidate result failed without a diagnostic")
            }
            Err(diagnostic) => Err(diagnostic),
        }
    }

    fn delegate_by_location(
        &self,
        site: &crate::fir::ResolvedSignatureDelegateSite,
    ) -> (crate::fir::SourceFileId, Span) {
        match self.headers.signature_origins.get(site.by_origin) {
            Some(crate::fir::Origin::Source { file, span }) => (file, span),
            Some(crate::fir::Origin::Synthetic { .. }) => {
                panic!("a delegated-property `by` origin must be an exact source origin")
            }
            None => panic!("a delegated-property `by` origin must remain in the signature arena"),
        }
    }

    fn delegate_convention_site(
        &self,
        site: &crate::fir::ResolvedSignatureDelegateSite,
        by_span: crate::diag::Span,
    ) -> crate::resolve::delegated_properties::DelegateConventionSite {
        let (dispatch_receiver, extension_receiver) = self.delegate_receivers(site);
        crate::resolve::delegated_properties::DelegateConventionSite {
            by_span,
            dispatch_receiver,
            extension_receiver,
            dispatch_source_name: site.dispatch_source_name.clone(),
            is_var: site.mutable,
        }
    }

    fn delegate_first_arguments(
        &self,
        site: &crate::fir::ResolvedSignatureDelegateSite,
    ) -> (Ty, Ty) {
        let (dispatch, extension) = self.delegate_receivers(site);
        let provide_ref = match dispatch {
            Some(dispatch) => dispatch,
            None => Ty::Null,
        };
        let this_ref = match (extension, dispatch) {
            (Some(extension), _) => extension,
            (None, Some(dispatch)) => dispatch,
            (None, None) => Ty::Null,
        };
        (provide_ref, this_ref)
    }

    fn delegate_receivers(
        &self,
        site: &crate::fir::ResolvedSignatureDelegateSite,
    ) -> (Option<Ty>, Option<Ty>) {
        let extension = |expected: bool| {
            expected.then(|| {
                self.extension_receivers
                    .get(&site.diagnostic_owner)
                    .expect("an extension delegate site must retain its resolved receiver")
                    .get()
            })
        };
        match site.kind {
            crate::fir::ResolvedSignatureDelegateSiteKind::TopLevel {
                extension: has_extension,
            } => (None, extension(has_extension)),
            crate::fir::ResolvedSignatureDelegateSiteKind::Member {
                dispatch,
                extension: has_extension,
            } => (Some(dispatch.get()), extension(has_extension)),
            crate::fir::ResolvedSignatureDelegateSiteKind::StatementLocal => (None, None),
        }
    }

    fn resolved_delegate_result(
        &self,
        site: &crate::fir::ResolvedSignatureDelegateSite,
        convention: &str,
        result: Ty,
    ) -> crate::fir::ResolvedTy {
        match crate::fir::ResolvedTy::new(result) {
            Ok(result) => result,
            Err(_) => panic!(
                "selected delegate convention {convention} at {:?} must have a resolved result",
                site.by_origin,
            ),
        }
    }

    fn delegate_result_or_invariant(
        &self,
        site: &crate::fir::ResolvedSignatureDelegateSite,
        convention: &str,
        result: Result<crate::fir::ResolvedTy, crate::fir::DiagnosticId>,
    ) -> Result<crate::fir::ResolvedTy, crate::fir::DiagnosticId> {
        match result {
            Err(diagnostic) if diagnostic.raw() == 0 => panic!(
                "delegate convention {convention} at {:?} failed without a diagnostic",
                site.by_origin,
            ),
            result => result,
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::symbol_source::{SymbolNamespace, SymbolSource};

    /// The language builtins, minus the one declaration a delegate convention is selected against.
    ///
    /// `kotlin.reflect.KProperty` is what a delegated property's accessors pass as the second
    /// convention argument, so a dependency set that declares none cannot supply the convention at
    /// all. This source models exactly that set: everything else answers as usual.
    struct WithoutPropertyReferences;

    impl SymbolSource for WithoutPropertyReferences {
        fn package_exists(&self, parent: crate::types::TypeName, name: &str) -> bool {
            name != "reflect" && crate::libraries::EmptySymbolSource.package_exists(parent, name)
        }

        fn symbols(
            &self,
            namespace: SymbolNamespace,
            name: &str,
        ) -> std::rc::Rc<crate::libraries::ResolvedSymbols> {
            match namespace {
                SymbolNamespace::Package(package) if package.matches("kotlin/reflect") => {
                    std::rc::Rc::new(crate::libraries::ResolvedSymbols::default())
                }
                _ => crate::libraries::EmptySymbolSource.symbols(namespace, name),
            }
        }
    }

    impl crate::libraries::SemanticPlatform for WithoutPropertyReferences {}

    fn frontend_diagnostics(
        source: &str,
        platform: Box<dyn crate::libraries::SemanticPlatform>,
    ) -> Vec<String> {
        let inputs = [crate::source::SourceInput::kotlin(source).with_file_stem("Held")];
        let mut diagnostics = crate::diag::DiagSink::new();
        let analysis = crate::frontend::analyze_source_set_with_features_and_prepare(
            &inputs,
            platform,
            &crate::features::LangFeatures::new(),
            |_, _| {},
            &mut diagnostics,
        );
        let _ = crate::compiler::check_frontend_only(analysis, &mut diagnostics);
        diagnostics
            .diags
            .iter()
            .map(|diagnostic| diagnostic.msg.clone())
            .collect()
    }

    /// The delegate convention is selected against a RESOLVED `KProperty`, never a spelling the
    /// compiler manufactures. A dependency set that declares none therefore refuses the property —
    /// with the ordinary convention diagnostic, not an internal failure and not a silent accept.
    #[test]
    fn a_dependency_set_without_kproperty_refuses_the_convention_instead_of_assuming_it() {
        const SOURCE: &str =
            "class Cache { operator fun getValue(owner: Any?, slot: Any?): Int = 1 }\n\
             val kept by Cache()\n";
        assert_eq!(
            frontend_diagnostics(SOURCE, Box::new(WithoutPropertyReferences)),
            vec![
                "property delegate must have a 'getValue(Nothing?, KProperty0<Int>)' method. None \
                 of the following functions is applicable:\nfun getValue(owner: Any?, slot: Any?): \
                 Int"
                .to_string(),
            ],
            "the missing classifier is reported, not invented",
        );
        assert_eq!(
            frontend_diagnostics(SOURCE, Box::new(crate::libraries::EmptySymbolSource)),
            Vec::<String>::new(),
            "the same source with the declaration present is accepted",
        );
    }
}
