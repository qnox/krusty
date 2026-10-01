//! The member level of one receiver rung during signature inference.
//!
//! Kotlin's member level for a receiver holds its member functions together with the
//! constructors of an `inner` classifier it exposes under the same spelling. Signature inference
//! mirrors the body checker: both form one candidate family, so argument expectations and result
//! selection run over functions and constructors at once and an ambiguity between them stays an
//! ambiguity.

use super::*;
use crate::symbol_source::SymbolSource;

impl ProductionSignatureSemantics<'_> {
    /// Collect `receiver`'s member level for `spelling`: the constructors of the inner classifier
    /// it binds, listed first as kotlinc lists them, then its callables.
    pub(super) fn receiver_member_level(
        &self,
        scope: crate::fir::SignatureScope,
        resolver: &crate::symbol_resolver::SymbolResolver<'_>,
        receiver: Ty,
        spelling: &str,
    ) -> Result<crate::libraries::Callables, crate::fir::DiagnosticId> {
        let (mut functions, properties) =
            resolver.receiver_callables(receiver, spelling).into_parts();
        let mut overloads = match self.bound_inner_classifier(scope, receiver, spelling)? {
            Some(internal) => resolver.bound_inner_constructor_candidates(receiver, internal),
            None => Vec::new(),
        };
        overloads.extend(
            self.implicit_context_candidates(scope, std::mem::take(&mut functions.overloads)),
        );
        functions.overloads = overloads;
        Ok(crate::libraries::Callables::from_parts(
            functions, properties,
        ))
    }

    /// Whether the implicit receiver that binds the inner classifier `internal` for a bare
    /// `spelling(args)` also declares same-named member functions. Its member level then decides
    /// the call over both families, ahead of the classifier tower's constructor-only selection.
    pub(super) fn implicit_member_level_owns_constructors(
        &self,
        scope: crate::fir::SignatureScope,
        internal: crate::types::TypeName,
        spelling: &str,
    ) -> bool {
        for receiver in self.implicit_receivers(scope) {
            match self.bound_inner_classifier(scope, receiver, spelling) {
                Ok(Some(bound)) if bound == internal => {
                    return self
                        .with_resolver(scope, |resolver| {
                            Some(
                                resolver
                                    .receiver_callables(receiver, spelling)
                                    .functions()
                                    .iter()
                                    .any(|function| {
                                        function.kind == crate::libraries::FnKind::Member
                                    }),
                            )
                        })
                        .unwrap_or(false);
                }
                Ok(_) => {}
                Err(_) => return false,
            }
        }
        false
    }

    /// Result of a member-level selection that chose an inner classifier's constructor: the
    /// selected, receiver-specialized constructor applied with `receiver` as its outer instance.
    pub(super) fn selected_inner_constructor_result(
        &self,
        scope: crate::fir::SignatureScope,
        receiver: Ty,
        selected: &crate::libraries::LibraryMember,
        argument_types: &[Ty],
        type_arguments: &[Ty],
    ) -> Result<crate::fir::ResolvedTy, crate::fir::DiagnosticId> {
        let owner = selected.owner.ok_or_else(Self::failure)?;
        let module = crate::module_symbols::ModuleSymbols::for_file(self.table, scope.source.raw());
        let source = crate::symbol_source::CompositeSource::new(vec![
            &module as &dyn crate::symbol_source::SymbolSource,
            &*self.table.libraries as &dyn crate::symbol_source::SymbolSource,
        ]);
        let classifier = source.classifier(owner).ok_or_else(Self::failure)?;
        let selected = crate::symbol_resolver::selected_constructor::capture(
            selected.clone(),
            classifier.as_ref(),
        );
        self.constructor_result(
            scope,
            &selected,
            argument_types,
            Some(receiver),
            type_arguments,
            None,
        )
    }

    /// Select an inner classifier inherited by a concrete outer receiver, then run its constructor
    /// through the ordinary resolver candidate family. This is the signature-graph counterpart of
    /// checked-body `outer.Inner(args)`/`super.Inner(args)` resolution; it returns only the compact
    /// result type and retains no constructor body or syntax identity.
    pub(super) fn bound_inner_classifier(
        &self,
        scope: crate::fir::SignatureScope,
        receiver: Ty,
        spelling: &str,
    ) -> Result<Option<crate::types::TypeName>, crate::fir::DiagnosticId> {
        let Some(outer) = receiver.kotlin_class_internal() else {
            return Ok(None);
        };
        let module = crate::module_symbols::ModuleSymbols::for_file(self.table, scope.source.raw());
        let source = crate::symbol_source::CompositeSource::new(vec![
            &module as &dyn crate::symbol_source::SymbolSource,
            &*self.table.libraries as &dyn crate::symbol_source::SymbolSource,
        ]);
        let selected = crate::symbol_resolver::inherited_nested_classifier_name(
            spelling,
            vec![outer],
            |owner| {
                crate::symbol_resolver::direct_supertypes(&source, Ty::obj_name(owner))
                    .into_iter()
                    .filter_map(Ty::kotlin_class_internal)
                    .collect()
            },
            |candidate| {
                crate::symbol_resolver::inherited_classifier_shape(&source, candidate, outer)
                    .is_some_and(|shape| shape.outer_instance.is_some())
            },
        );
        match selected {
            crate::symbol_resolver::InheritedNestedClassifier::NotFound => Ok(None),
            crate::symbol_resolver::InheritedNestedClassifier::Ambiguous => Err(Self::failure()),
            crate::symbol_resolver::InheritedNestedClassifier::Found(internal) => {
                Ok(Some(internal))
            }
        }
    }

    pub(super) fn bound_inner_constructor_result(
        &self,
        scope: crate::fir::SignatureScope,
        receiver: Ty,
        spelling: &str,
        arguments: &[crate::fir::ResolvedSigCallArgument<'_>],
        type_arguments: &[Ty],
    ) -> Result<Option<crate::fir::ResolvedTy>, crate::fir::DiagnosticId> {
        let Some(internal) = self.bound_inner_classifier(scope, receiver, spelling)? else {
            return Ok(None);
        };
        let argument_kinds = arguments
            .iter()
            .map(Self::call_argument_kind)
            .collect::<Vec<_>>();
        let argument_types = arguments
            .iter()
            .map(|argument| argument.ty.get())
            .collect::<Vec<_>>();
        let declaration = self.with_resolver(scope, |resolver| {
            resolver.select_constructor_declaration_with_type_arguments(
                internal,
                &argument_kinds,
                type_arguments,
            )
        })?;
        self.constructor_result(
            scope,
            &declaration,
            &argument_types,
            Some(receiver),
            type_arguments,
            None,
        )
        .map(Some)
    }
}
