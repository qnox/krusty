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
        declaration: crate::fir::DeclarationId,
        scope: crate::fir::SignatureScope,
        origin: crate::fir::OriginId,
        delegate: crate::fir::ResolvedTy,
        local: bool,
        demand: &mut dyn FnMut(
            crate::fir::DeclarationId,
        )
            -> Result<crate::fir::ResolvedSignature, crate::fir::DiagnosticId>,
    ) -> Result<crate::fir::ResolvedTy, crate::fir::DiagnosticId> {
        let this_ref = if local {
            Ty::Null
        } else {
            self.delegate_this_ref(declaration)
        };
        // The classifier is resolved through the same source that answers applicability, so this
        // probe and the checked selection agree on which declaration `KProperty` is — and a
        // dependency set that declares none answers `None` here, exactly as a missing convention
        // does.
        let property_reference = match self.with_resolver(scope, |resolver| {
            crate::resolve::delegated_properties::delegate_property_reference_type(resolver)
        }) {
            Ok(property_reference) => property_reference,
            // Without the classifier the accessors pass there is no convention to select against,
            // which is the same refusal a missing `getValue` is and is reported the same way. The
            // alternative — manufacturing the spelling — selects against a declaration the
            // dependency set does not have.
            Err(_) => {
                return Err(self.refuse_missing_delegate_convention(
                    declaration,
                    scope,
                    origin,
                    delegate.get(),
                    this_ref,
                    local,
                    demand,
                ));
            }
        };
        let arguments = [this_ref, property_reference];
        let provided = self.with_resolver(scope, |resolver| {
            Some(super::super::select_delegate_operator(
                resolver,
                delegate.get(),
                "provideDelegate",
                &arguments,
            ))
        })?;
        crate::trace_compiler!(
            "signature",
            "delegate provide receiver={:?} selection={:?}",
            delegate.get(),
            match &provided {
                crate::symbol_resolver::CandidateSelection::Selected((selected, result)) => Some((
                    selected.semantic_receiver(),
                    selected.semantic_params(),
                    *result,
                )),
                crate::symbol_resolver::CandidateSelection::None
                | crate::symbol_resolver::CandidateSelection::Ambiguous => None,
            },
        );
        let stored = match provided {
            crate::symbol_resolver::CandidateSelection::None => {
                let selected = self
                    .implicit_receivers(scope)
                    .into_iter()
                    .find_map(|dispatch| {
                        self.signature_member_extension_call(
                            scope,
                            delegate.get(),
                            dispatch,
                            "provideDelegate",
                            super::lookups::SignatureMemberExtensionArguments {
                                types: &arguments,
                                ..Default::default()
                            },
                            None,
                            super::super::MemberExtensionSelection::Operators,
                        )
                    });
                match selected {
                    Some((_result, Some(declaration)))
                        if self
                            .headers
                            .stub(declaration)
                            .is_some_and(|stub| stub.signature_inference.is_some()) =>
                    {
                        demand(declaration)?.result
                    }
                    Some((result, _)) => {
                        crate::fir::ResolvedTy::new(result).map_err(|_| Self::failure())?
                    }
                    None => delegate,
                }
            }
            crate::symbol_resolver::CandidateSelection::Ambiguous => return Err(Self::failure()),
            crate::symbol_resolver::CandidateSelection::Selected((selected, result)) => self
                .selected_convention_result(
                    delegate.get(),
                    &selected,
                    result,
                    &arguments,
                    demand,
                )?,
        };
        let selected = self.with_resolver(scope, |resolver| {
            Some(super::super::select_delegate_operator(
                resolver,
                stored.get(),
                "getValue",
                &arguments,
            ))
        })?;
        crate::trace_compiler!(
            "signature",
            "delegate get receiver={:?} selection={:?}",
            stored.get(),
            match &selected {
                crate::symbol_resolver::CandidateSelection::Selected((selected, result)) => Some((
                    selected.semantic_receiver(),
                    selected.semantic_params(),
                    *result,
                )),
                crate::symbol_resolver::CandidateSelection::None
                | crate::symbol_resolver::CandidateSelection::Ambiguous => None,
            },
        );
        match selected {
            crate::symbol_resolver::CandidateSelection::Selected((selected, result)) => {
                self.selected_convention_result(stored.get(), &selected, result, &arguments, demand)
            }
            crate::symbol_resolver::CandidateSelection::None => {
                for dispatch in self.implicit_receivers(scope) {
                    let Some((result, declaration)) = self.signature_member_extension_call(
                        scope,
                        stored.get(),
                        dispatch,
                        "getValue",
                        super::lookups::SignatureMemberExtensionArguments {
                            types: &arguments,
                            ..Default::default()
                        },
                        None,
                        super::super::MemberExtensionSelection::Operators,
                    ) else {
                        continue;
                    };
                    if let Some(declaration) = declaration {
                        if self
                            .headers
                            .stub(declaration)
                            .is_some_and(|stub| stub.signature_inference.is_some())
                        {
                            return demand(declaration).map(|signature| signature.result);
                        }
                    }
                    return crate::fir::ResolvedTy::new(result).map_err(|_| Self::failure());
                }
                // A property with NO declared type has nothing left to infer its type FROM once
                // `getValue` is missing, so this refusal is where the source mistake is finally
                // known. Declining silently made signature finalization fail with no cause named,
                // which skips body checking for the whole file — the file then reported nothing at
                // all, including its unrelated diagnostics. Name it here, in the same wording the
                // body check uses, so the two collapse wherever both reach the sink.
                Err(self.refuse_missing_delegate_convention(
                    declaration,
                    scope,
                    origin,
                    stored.get(),
                    this_ref,
                    local,
                    demand,
                ))
            }
            crate::symbol_resolver::CandidateSelection::Ambiguous => Err(Self::failure()),
        }
    }
    /// The `thisRef` passed to delegated-property conventions belongs to the property declaration,
    /// not to the last receiver in its scope tower. An extension property's receiver wins; an
    /// ordinary member uses its nearest classifier owner; a top-level property has a null receiver.
    fn delegate_this_ref(&self, declaration: crate::fir::DeclarationId) -> Ty {
        if let Some(receiver) = self.extension_receivers.get(&declaration) {
            return receiver.get();
        }
        self.enclosing_classifier_self(declaration)
            .unwrap_or(Ty::Null)
    }

    /// The `this` type of the classifier this declaration is lexically inside, if any.
    fn enclosing_classifier_self(&self, declaration: crate::fir::DeclarationId) -> Option<Ty> {
        let mut owner = self
            .headers
            .declarations
            .anchor(declaration)
            .and_then(|anchor| anchor.owner);
        while let Some(declaration) = owner {
            let anchor = self.headers.declarations.anchor(declaration)?;
            if anchor.kind == crate::fir::DeclarationKind::Classifier {
                return Some(
                    self.classifier_signature(declaration)
                        .map(semantic_classifier_self)
                        .unwrap_or(Ty::Null),
                );
            }
            owner = anchor.owner;
        }
        None
    }

    /// Record the missing-`getValue` diagnostic for a delegated property and answer its identity.
    ///
    /// The identity is what makes signature finalization publish it: a failure carrying one is
    /// reported, a bare [`Self::failure`] is not.
    fn refuse_missing_delegate_convention(
        &self,
        declaration: crate::fir::DeclarationId,
        scope: crate::fir::SignatureScope,
        origin: crate::fir::OriginId,
        delegate: Ty,
        this_ref: Ty,
        local: bool,
        demand: &mut dyn FnMut(
            crate::fir::DeclarationId,
        )
            -> Result<crate::fir::ResolvedSignature, crate::fir::DiagnosticId>,
    ) -> crate::fir::DiagnosticId {
        let Some(by_span) = self.delegate_by_span(declaration, origin) else {
            return Self::failure();
        };
        let site = self.delegate_convention_site(declaration, local, by_span);
        // A candidate's own return may still be undetermined here — this phase is what determines
        // it — and rendering the placeholder produces a second, differently worded message for the
        // same mistake once the body check reports it. Ask the solver for the declaration instead,
        // exactly as a SELECTED convention's result is asked for.
        let message = self.with_resolver(scope, |resolver| {
            crate::resolve::delegated_properties::delegate_convention_message(
                resolver,
                site,
                delegate,
                "getValue",
                this_ref,
                None,
                None,
                &mut |candidate| self.determined_candidate_result(candidate, demand),
            )
        });
        match message {
            Ok(message) => {
                self.record_source_diagnostic_at(declaration, scope.source, by_span, message)
            }
            Err(identity) => identity,
        }
    }

    /// One candidate's result type, resolved when this phase has not determined it yet.
    ///
    /// A declaration whose own return is inferred reads as undetermined in the symbol table until
    /// its signature is solved. The solver answers for it on demand; a refusal to answer leaves the
    /// recorded type, which is the most the message can say.
    fn determined_candidate_result(
        &self,
        candidate: &crate::libraries::FunctionInfo,
        demand: &mut dyn FnMut(
            crate::fir::DeclarationId,
        )
            -> Result<crate::fir::ResolvedSignature, crate::fir::DiagnosticId>,
    ) -> Ty {
        let recorded = candidate.callable.ret;
        if !recorded.mentions_pending() {
            return recorded;
        }
        self.demanded_member_signature(candidate.stable_declaration, demand)
            .ok()
            .flatten()
            .or_else(|| {
                self.demanded_source_signature(None, candidate.stable_declaration, demand)
                    .ok()
                    .flatten()
            })
            .map_or(recorded, |signature| signature.result.get())
    }

    /// Where a delegate-convention diagnostic about this property points.
    ///
    /// kotlinc anchors it on the `by` keyword, which the retained header records for the delegate
    /// operation itself; a synthetic origin defers to the cause it was produced from, and a
    /// declaration with neither has no place to point and reports nothing.
    fn delegate_by_span(
        &self,
        declaration: crate::fir::DeclarationId,
        origin: crate::fir::OriginId,
    ) -> Option<Span> {
        let _ = declaration;
        match self.headers.signature_origins.get(origin) {
            Some(crate::fir::Origin::Source { span, .. }) => Some(span),
            Some(crate::fir::Origin::Synthetic { cause, .. }) => {
                match self.headers.signature_origins.get(cause) {
                    Some(crate::fir::Origin::Source { span, .. }) => Some(span),
                    Some(crate::fir::Origin::Synthetic { .. }) | None => None,
                }
            }
            None => None,
        }
    }

    /// What a delegate-convention diagnostic about this property names beyond the types in hand.
    ///
    /// A LOCAL delegated property has no receiver of any kind: its accessors pass `null` as
    /// `thisRef` and a `KProperty0` reference, whatever encloses the function it is declared in.
    fn delegate_convention_site(
        &self,
        declaration: crate::fir::DeclarationId,
        local: bool,
        by_span: crate::diag::Span,
    ) -> crate::resolve::delegated_properties::DelegateConventionSite {
        let mutable = self
            .headers
            .syntax
            .declaration(declaration)
            .is_some_and(|header| {
                matches!(
                    header.kind,
                    crate::fir::HeaderDeclarationKind::Property { mutable: true, .. }
                )
            });
        crate::resolve::delegated_properties::DelegateConventionSite {
            by_span,
            dispatch_receiver: (!local)
                .then(|| self.enclosing_classifier_self(declaration))
                .flatten(),
            extension_receiver: (!local)
                .then(|| self.extension_receivers.get(&declaration))
                .flatten()
                .map(|receiver| receiver.get()),
            is_var: mutable,
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
