//! The member level of one receiver rung during signature inference.
//!
//! Kotlin's member level for a receiver holds its member functions together with the
//! constructors of an `inner` classifier it exposes under the same spelling. Signature inference
//! mirrors the body checker: both form one candidate family, so argument expectations and result
//! selection run over functions and constructors at once and an ambiguity between them stays an
//! ambiguity.

use super::*;
use crate::symbol_source::SymbolSource;

pub(super) struct ReceiverMemberLevel {
    pub(super) callables: crate::libraries::Callables,
    /// Unprojected source declarations keyed by stable identity. Rejected recovery uses this
    /// source shape when it must demand an inferred return, retaining the applied enclosing
    /// dispatch receiver and then applying any context projection exactly once.
    pub(super) candidate_sources: Vec<ReceiverCandidateSource>,
    /// Exact declarations excluded only because one of their context parameters is unavailable.
    pub(super) unavailable_context: Vec<crate::libraries::FunctionInfo>,
}

pub(super) struct ReceiverCandidateSource {
    pub(super) candidate: crate::libraries::FunctionInfo,
    /// Applied enclosing instance that owns a member extension. A top-level extension has no
    /// dispatch receiver and specializes only from its explicit extension receiver.
    pub(super) dispatch_receiver: Option<Ty>,
    /// Exact member-extension tower priority retained from the provider shape. Rejected-call
    /// recovery applies the same override/inheritance ordering as ordinary selection before the
    /// shared callable-specificity pass.
    pub(super) member_extension_priority: Option<super::super::MemberExtensionPriority>,
}

/// Whether two projected candidate records denote the same provider declaration. Projection and
/// receiver specialization may change their types, so recovery keys on the stable identity each
/// provider publishes and uses the complete declaration coordinate only for synthetic records.
pub(super) fn same_candidate_declaration(
    left: &crate::libraries::FunctionInfo,
    right: &crate::libraries::FunctionInfo,
) -> bool {
    if let (Some(left), Some(right)) = (left.stable_declaration, right.stable_declaration) {
        return left == right;
    }
    if let (Some(left), Some(right)) = (left.source_member, right.source_member) {
        return left == right;
    }
    if let (Some(left), Some(right)) = (
        left.callable.external_identity,
        right.callable.external_identity,
    ) {
        return left == right;
    }
    left.kind == right.kind
        && left.callable.declaration_owner == right.callable.declaration_owner
        && left.callable.owner == right.callable.owner
        && left.callable.name == right.callable.name
        && left.callable.descriptor == right.callable.descriptor
}

impl ProductionSignatureSemantics<'_> {
    /// Member-extension declarations visible through the implicit dispatch tower, before context
    /// applicability or value-argument applicability is applied. Rejected-call recovery needs this
    /// declaration family when ordinary selection rejects an argument: `receiver_callables` owns
    /// explicit-receiver members and lexical extensions, while member extensions are declarations
    /// of an implicit dispatch receiver and therefore live in this separate tower.
    pub(super) fn implicit_member_extension_candidate_sources(
        &self,
        scope: crate::fir::SignatureScope,
        extension_receiver: Ty,
        spelling: &str,
    ) -> Vec<ReceiverCandidateSource> {
        let module = crate::module_symbols::ModuleSymbols::for_file(self.table, scope.source.raw());
        let source = crate::symbol_source::CompositeSource::new(vec![
            &module as &dyn crate::symbol_source::SymbolSource,
            &*self.table.libraries as &dyn crate::symbol_source::SymbolSource,
        ]);
        let receivers = self
            .signature_dispatch_receivers(scope)
            .into_iter()
            .map(super::super::ImplicitReceiver::signature_receiver)
            .collect::<Vec<_>>();
        super::super::member_extension_function_shapes_in(
            &source,
            &receivers,
            extension_receiver,
            spelling,
        )
        .into_iter()
        .map(|shape| {
            let dispatch_receiver = shape.dispatch_receiver.ty;
            ReceiverCandidateSource {
                candidate: shape.diagnostic_candidate,
                dispatch_receiver: Some(dispatch_receiver),
                member_extension_priority: Some(shape.priority),
            }
        })
        .collect()
    }

    /// Collect `receiver`'s member level for `spelling`: the constructors of the inner classifier
    /// it binds, listed first as kotlinc lists them, then its callables.
    pub(super) fn receiver_member_level(
        &self,
        scope: crate::fir::SignatureScope,
        resolver: &crate::symbol_resolver::SymbolResolver<'_>,
        receiver: Ty,
        spelling: &str,
    ) -> Result<crate::libraries::Callables, crate::fir::DiagnosticId> {
        self.receiver_member_level_with_context_status(scope, resolver, receiver, spelling)
            .map(|level| level.callables)
    }

    /// Collect the member level while retaining declarations excluded solely by unavailable
    /// context. The ordinary selector consumes `callables`; rejected-call recovery consumes both
    /// families so a missing context never turns an existing declaration into an unresolved name.
    pub(super) fn receiver_member_level_with_context_status(
        &self,
        scope: crate::fir::SignatureScope,
        resolver: &crate::symbol_resolver::SymbolResolver<'_>,
        receiver: Ty,
        spelling: &str,
    ) -> Result<ReceiverMemberLevel, crate::fir::DiagnosticId> {
        self.receiver_member_level_with_context_status_and_constraints(
            scope,
            resolver,
            receiver,
            spelling,
            &[],
            None,
        )
    }

    pub(super) fn receiver_member_level_with_context_status_and_constraints(
        &self,
        scope: crate::fir::SignatureScope,
        resolver: &crate::symbol_resolver::SymbolResolver<'_>,
        receiver: Ty,
        spelling: &str,
        type_arguments: &[Ty],
        expected: Option<Ty>,
    ) -> Result<ReceiverMemberLevel, crate::fir::DiagnosticId> {
        let (mut functions, properties) =
            resolver.receiver_callables(receiver, spelling).into_parts();
        let dispatch_receivers = self.signature_dispatch_receivers(scope);
        let candidate_sources = functions
            .overloads
            .iter()
            .cloned()
            .map(|candidate| {
                let dispatch_receiver = candidate.is_extension().then(|| {
                    dispatch_receivers.iter().copied().find(|receiver| {
                        self.table
                            .applied_type_hierarchy(receiver.non_null())
                            .into_iter()
                            .any(|(owner, _, _)| owner == candidate.callable.owner)
                    })
                });
                ReceiverCandidateSource {
                    candidate,
                    dispatch_receiver: dispatch_receiver.flatten(),
                    member_extension_priority: None,
                }
            })
            .collect();
        let mut overloads = match self.bound_inner_classifier(scope, receiver, spelling)? {
            Some(internal) => resolver.bound_inner_constructor_candidates(receiver, internal),
            None => Vec::new(),
        };
        let (available, unavailable_context) = self
            .partition_implicit_context_candidates_with_constraints(
                scope,
                std::mem::take(&mut functions.overloads),
                type_arguments,
                expected,
            );
        overloads.extend(available);
        functions.overloads = overloads;
        Ok(ReceiverMemberLevel {
            callables: crate::libraries::Callables::from_parts(functions, properties),
            candidate_sources,
            unavailable_context,
        })
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
