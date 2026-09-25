//! Compact signature solving for classifier-associated declarations: `companion { … }` block
//! members and written `companion fun/val C.name`.
//!
//! These consume the provider queries the checker uses (`SymbolResolver::classifier_associated_*`
//! and `static_scope_associated_*`) and select a function with the ordinary receiver-less
//! top-level selector, so the compact path and Pass 2 agree on which declaration a call names.

use super::*;

/// The outcome of one associated rung in compact signature solving.
pub(super) enum AssociatedSignatureCall {
    /// The rung declares no associated function with this name.
    Absent,
    /// Candidates exist but none is applicable.
    Inapplicable,
    /// The selected declaration's result.
    Selected(crate::fir::ResolvedTy),
}

/// One rung of the unqualified call/value tower: an implicit value receiver or a classifier's
/// static scope, in lexical order.
pub(super) enum ImplicitRung {
    Receiver(Ty),
    StaticScope(crate::types::TypeName),
}

/// Arguments of one compact call, as the ordinary call path receives them.
pub(super) struct AssociatedSignatureArguments<'call, 'argument> {
    pub(super) arguments: &'call [crate::fir::ResolvedSigCallArgument<'argument>],
    pub(super) type_arguments: &'call [crate::fir::ResolvedTy],
    pub(super) trailing_lambda: bool,
    pub(super) expected: Option<crate::fir::ResolvedTy>,
}

type Demand<'demand> = dyn FnMut(
        crate::fir::DeclarationId,
    ) -> Result<crate::fir::ResolvedSignature, crate::fir::DiagnosticId>
    + 'demand;

impl ProductionSignatureSemantics<'_> {
    /// The classifier a companion-associated declaration is associated with.
    pub(super) fn declaration_associated_classifier(
        &self,
        declaration: crate::fir::DeclarationId,
    ) -> Option<crate::types::TypeName> {
        let function = self
            .table
            .ext_funs
            .values()
            .flat_map(HashMap::values)
            .flatten()
            .find(|signature| signature.stable_declaration == Some(declaration))
            .filter(|signature| signature.is_companion_extension())
            .and_then(|signature| signature.source_receiver);
        let property = || {
            self.table
                .ext_props
                .values()
                .flatten()
                .find(|property| property.stable_declaration == Some(declaration))
                .filter(|property| property.is_companion_extension)
                .map(|property| property.receiver)
        };
        function
            .or_else(property)
            .and_then(|receiver| receiver.non_null().obj_internal())
    }

    /// Classifiers whose static scope is open at `scope`, innermost first: the classifier of an
    /// enclosing companion-associated declaration, then the enclosing classes.
    fn static_scope_classifiers(
        &self,
        scope: crate::fir::SignatureScope,
    ) -> Vec<crate::types::TypeName> {
        let mut classifiers = Vec::new();
        let mut owner = Some(scope.owner);
        while let Some(declaration) = owner {
            if let Some(classifier) = self.declaration_associated_classifier(declaration) {
                if !classifiers.contains(&classifier) {
                    classifiers.push(classifier);
                }
            }
            owner = self.declaration_semantic_parent(declaration);
        }
        for classifier in self.lexical_class_names(scope) {
            if !classifiers.contains(&classifier) {
                classifiers.push(classifier);
            }
        }
        classifiers
    }

    /// The implicit receivers of `scope` interleaved with the static scopes of the classifiers
    /// open there: a class's static scope follows its own instance receiver, and a static scope
    /// with no receiver in lexical scope follows every receiver.
    pub(super) fn implicit_rungs(&self, scope: crate::fir::SignatureScope) -> Vec<ImplicitRung> {
        let mut pending = self.static_scope_classifiers(scope);
        let mut rungs = Vec::new();
        for receiver in self
            .implicit_receivers(scope)
            .into_iter()
            .chain(self.enclosing_lexical_singleton_receivers(scope))
        {
            rungs.push(ImplicitRung::Receiver(receiver));
            if let Some(index) = receiver.non_null().obj_internal().and_then(|internal| {
                pending
                    .iter()
                    .position(|classifier| *classifier == internal)
            }) {
                rungs.push(ImplicitRung::StaticScope(pending.remove(index)));
            }
        }
        rungs.extend(pending.into_iter().map(ImplicitRung::StaticScope));
        rungs
    }

    /// `C.name(args)` naming `classifier`'s own associated functions.
    pub(super) fn select_qualified_associated_call(
        &self,
        scope: crate::fir::SignatureScope,
        classifier: crate::types::TypeName,
        spelling: &str,
        call: &AssociatedSignatureArguments<'_, '_>,
        demand: &mut Demand<'_>,
    ) -> Result<AssociatedSignatureCall, crate::fir::DiagnosticId> {
        let candidates = self
            .with_resolver(scope, |resolver| {
                Some(resolver.classifier_associated_callables(classifier, spelling))
            })
            .unwrap_or_default();
        self.select_associated_call(scope, spelling, candidates, call, demand)
    }

    /// `name(args)` resolved in `classifier`'s static scope: its own and its supertypes'
    /// associated functions, the nearest applicable classifier winning.
    pub(super) fn select_static_scope_call(
        &self,
        scope: crate::fir::SignatureScope,
        classifier: crate::types::TypeName,
        spelling: &str,
        call: &AssociatedSignatureArguments<'_, '_>,
        demand: &mut Demand<'_>,
    ) -> Result<AssociatedSignatureCall, crate::fir::DiagnosticId> {
        let candidates = self
            .with_resolver(scope, |resolver| {
                Some(resolver.static_scope_associated_callables(classifier, spelling))
            })
            .unwrap_or_default();
        self.select_associated_call(scope, spelling, candidates, call, demand)
    }

    /// Select among associated `candidates` one supertype distance at a time with the ordinary
    /// receiver-less selector, then demand the selected source declaration's signature.
    fn select_associated_call(
        &self,
        scope: crate::fir::SignatureScope,
        spelling: &str,
        candidates: Vec<crate::libraries::FunctionInfo>,
        call: &AssociatedSignatureArguments<'_, '_>,
        demand: &mut Demand<'_>,
    ) -> Result<AssociatedSignatureCall, crate::fir::DiagnosticId> {
        if candidates.is_empty() {
            return Ok(AssociatedSignatureCall::Absent);
        }
        let candidates = self.implicit_context_candidates(scope, candidates);
        let mut ranks = candidates
            .iter()
            .map(|candidate| candidate.receiver_rank)
            .collect::<Vec<_>>();
        ranks.sort_unstable();
        ranks.dedup();
        let resolved_type_arguments = call
            .type_arguments
            .iter()
            .map(|argument| argument.get())
            .collect::<Vec<_>>();
        let expected = call.expected.map(crate::fir::ResolvedTy::get);
        for rank in ranks {
            let tier = candidates
                .iter()
                .filter(|candidate| candidate.receiver_rank == rank)
                .cloned()
                .collect::<Vec<_>>();
            let Some((argument_kinds, argument_types)) =
                Self::mapped_call_arguments(&tier, call.arguments, call.trailing_lambda)
            else {
                continue;
            };
            let Ok((selected, callable)) = self.with_resolver(scope, |resolver| {
                resolver.select_top_level_function_candidates_with_expected(
                    spelling,
                    tier,
                    &argument_kinds,
                    &resolved_type_arguments,
                    expected,
                )
            }) else {
                continue;
            };
            if let Some(source) = selected.source_key {
                if let Some(signature) =
                    self.demanded_source_signature(None, selected.stable_declaration, demand)?
                {
                    let context_count = selected.context_count.min(signature.parameters.len());
                    let parameters = signature.parameters[context_count..]
                        .iter()
                        .map(|parameter| parameter.get())
                        .collect::<Vec<_>>();
                    self.record_scoped_argument_constraints(scope, &parameters, &argument_types);
                    return self
                        .apply_demanded_source_callable(
                            source,
                            None,
                            &signature,
                            &argument_types,
                            None,
                            &resolved_type_arguments,
                            expected,
                        )
                        .map(AssociatedSignatureCall::Selected);
                }
            }
            return crate::fir::ResolvedTy::new(callable.ret)
                .map(AssociatedSignatureCall::Selected)
                .map_err(|_| Self::failure());
        }
        Ok(AssociatedSignatureCall::Inapplicable)
    }

    /// `C.name` naming `classifier`'s own associated property.
    pub(super) fn select_qualified_associated_property(
        &self,
        scope: crate::fir::SignatureScope,
        classifier: crate::types::TypeName,
        spelling: &str,
        demand: &mut Demand<'_>,
    ) -> Result<Option<crate::fir::ResolvedTy>, crate::fir::DiagnosticId> {
        let properties = self
            .with_resolver(scope, |resolver| {
                Some(resolver.classifier_associated_properties(classifier, spelling))
            })
            .unwrap_or_default();
        self.associated_property_result(properties, demand)
    }

    /// `name` read in `classifier`'s static scope: the nearest classifier's associated property.
    pub(super) fn select_static_scope_property(
        &self,
        scope: crate::fir::SignatureScope,
        classifier: crate::types::TypeName,
        spelling: &str,
        demand: &mut Demand<'_>,
    ) -> Result<Option<crate::fir::ResolvedTy>, crate::fir::DiagnosticId> {
        let properties = self
            .with_resolver(scope, |resolver| {
                Some(resolver.static_scope_associated_properties(classifier, spelling))
            })
            .unwrap_or_default();
        self.associated_property_result(properties, demand)
    }

    fn associated_property_result(
        &self,
        properties: Vec<crate::libraries::PropertyInfo>,
        demand: &mut Demand<'_>,
    ) -> Result<Option<crate::fir::ResolvedTy>, crate::fir::DiagnosticId> {
        let Some(nearest) = properties
            .iter()
            .map(|property| property.receiver_rank)
            .min()
        else {
            return Ok(None);
        };
        let mut nearest = properties
            .into_iter()
            .filter(|property| property.receiver_rank == nearest);
        let (Some(property), None) = (nearest.next(), nearest.next()) else {
            return Err(Self::failure());
        };
        if let Some(signature) =
            self.demanded_source_signature(None, property.stable_declaration, demand)?
        {
            return Ok(Some(signature.result));
        }
        crate::fir::ResolvedTy::new(property.ty)
            .map(Some)
            .map_err(|_| Self::failure())
    }
}
