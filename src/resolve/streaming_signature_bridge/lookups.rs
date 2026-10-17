//! Resolver-backed classifier, type, and member-extension lookups for signature solving.

use super::super::alias_projection::{star_projected_alias_type, AliasArguments};
use super::*;

impl ProductionSignatureSemantics<'_> {
    /// The classifier a call spelling constructs: `lexical` when a nearer rung bound it, else the
    /// file/import tower's own selection. Either carries the alias declaration that named it.
    pub(super) fn constructed_classifier(
        resolver: &crate::symbol_resolver::SymbolResolver<'_>,
        lexical: Option<crate::symbol_resolver::ScopedClassifier>,
        spelling: &str,
    ) -> Option<crate::symbol_resolver::ScopedClassifier> {
        lexical.or_else(|| match resolver.scoped_classifier_in_scope(spelling) {
            crate::symbol_resolver::CandidateSelection::Selected(selected) => Some(selected),
            crate::symbol_resolver::CandidateSelection::Ambiguous
            | crate::symbol_resolver::CandidateSelection::None => None,
        })
    }

    /// The classifier a call spelling constructs, with the alias declaration that named it on the
    /// winning scope-tower rung. A classifier already bound to its declaration names no alias.
    pub(super) fn bound_or_scoped_classifier(
        &self,
        scope: crate::fir::SignatureScope,
        spelling: &str,
        bound: Option<crate::fir::DeclarationId>,
    ) -> Option<crate::symbol_resolver::ScopedClassifier> {
        match bound {
            Some(declaration) => Some(crate::symbol_resolver::ScopedClassifier {
                classifier: self.bound_classifier_identity(declaration),
                alias: None,
            }),
            None => self.qualified_scoped_classifier_binding(scope, spelling).0,
        }
    }

    /// The classifier that owns a declaration whose syntax is physically duplicated from the
    /// primary constructor header. Its nested declarations and type parameters are lexically
    /// visible there even though no dispatch receiver exists yet. Nested classifiers inherited by
    /// the owner form a distinct fallback rung after file/import lookup.
    pub(super) fn classifier_header_scope_owner(
        &self,
        declaration: crate::fir::DeclarationId,
    ) -> Option<crate::fir::DeclarationId> {
        let anchor = self.headers.declarations.anchor(declaration)?;
        let owner = anchor.owner?;
        let owner_header = self.headers.syntax.declaration(owner)?;
        let crate::fir::HeaderDeclarationKind::Classifier {
            primary_parameters, ..
        } = owner_header.kind
        else {
            return None;
        };
        match anchor.kind {
            crate::fir::DeclarationKind::Constructor if anchor.sibling == 0 => Some(owner),
            crate::fir::DeclarationKind::Property => self
                .headers
                .syntax
                .parameters(primary_parameters)
                .get(anchor.sibling as usize)
                .filter(|parameter| parameter.flags.is_property() && parameter.span == anchor.range)
                .map(|_| owner),
            crate::fir::DeclarationKind::Function
            | crate::fir::DeclarationKind::Classifier
            | crate::fir::DeclarationKind::TypeAlias
            | crate::fir::DeclarationKind::Accessor
            | crate::fir::DeclarationKind::Initializer
            | crate::fir::DeclarationKind::EnumEntry
            | crate::fir::DeclarationKind::Script
            | crate::fir::DeclarationKind::Constructor => None,
        }
    }

    /// A contextual lambda has the function type requested by its call site once its body result is
    /// accepted by the ordinary subtype relation. An unresolved expected type parameter remains an
    /// inference target, so its concrete body result continues through the compact constraint graph.
    pub(super) fn checked_contextual_function_result(
        &self,
        declaration: crate::fir::DeclarationId,
        actual: crate::fir::ResolvedTy,
        expected: crate::fir::ResolvedTy,
    ) -> Result<crate::fir::ResolvedTy, crate::fir::DiagnosticId> {
        if expected.get() == Ty::Unit {
            return Ok(expected);
        }
        if expected.get().mentions_ty_param() {
            return Ok(actual);
        }
        let source_file = self
            .headers
            .declarations
            .anchor(declaration)
            .map(|anchor| anchor.source.raw())
            .unwrap_or(0);
        let module = crate::module_symbols::ModuleSymbols::for_file(self.table, source_file);
        let source = crate::symbol_source::CompositeSource::new(vec![
            &module as &dyn crate::symbol_source::SymbolSource,
            &*self.table.libraries as &dyn crate::symbol_source::SymbolSource,
        ]);
        let oracle = crate::symbol_resolver::SourceOracle(&source);
        let context = crate::assignable::TyCtx::new();
        crate::assignable::is_subtype(&context, &oracle, actual.get(), expected.get())
            .then_some(expected)
            .ok_or_else(Self::failure)
    }

    pub(super) fn signature_type_ref_at_impl(
        &self,
        scope: crate::fir::SignatureScope,
        lexical: &super::super::CheckerScope<'_>,
        reference: &TypeRef,
        include_scope_owner_body: bool,
        selected_site: Option<SelectedTypeSite>,
    ) -> Option<Ty> {
        if reference.name.is_empty() {
            return None;
        }
        if lexical.tparam_contains(&reference.name) {
            let bound = lexical.tparam_bound(&reference.name);
            return Some(if reference.definitely_non_null() {
                super::super::definitely_non_null_ty(bound)
            } else if reference.nullable() {
                Ty::nullable(bound)
            } else {
                bound
            });
        }
        // Arrow syntax is already a complete semantic function type. Its parser-internal `<fun>`
        // spelling is not a classifier lookup key: rebinding it through the normalized FunctionN
        // inventory erases receiver, context, and suspend shape. Resolve the components in the
        // same lexical scope before considering aliases or classifiers.
        if super::super::function_type_ref_shape(reference).is_some() {
            for component in reference
                .fun_params
                .iter()
                .chain(reference.arg.iter().map(Box::as_ref))
                .filter(|component| component.is_star_projection())
            {
                self.record_type_spelling_projection(
                    self.selected_type_child(selected_site, reference, component),
                    Ty::nullable(Ty::obj("kotlin/Any")),
                );
            }
            let mut unresolved = false;
            let function = super::super::typeref_leaf(reference, &mut |component| match self
                .signature_type_ref_at_with_site(
                    scope,
                    lexical,
                    component,
                    include_scope_owner_body,
                    self.selected_type_child(selected_site, reference, component),
                ) {
                Some(ty) => ty,
                None => {
                    unresolved = true;
                    Ty::Error
                }
            });
            let function = function.filter(|_| !unresolved)?;
            return Some(if reference.nullable() {
                Ty::nullable(function)
            } else {
                function
            });
        }
        let associated_receiver = self
            .headers
            .stub(scope.owner)
            .is_some_and(|stub| stub.flags.has(crate::fir::DeclarationFlags::COMPANION))
            && self
                .headers
                .syntax
                .declaration(scope.owner)
                .and_then(|declaration| match declaration.kind {
                    crate::fir::HeaderDeclarationKind::Callable { receiver, .. }
                    | crate::fir::HeaderDeclarationKind::Property { receiver, .. } => receiver,
                    crate::fir::HeaderDeclarationKind::Classifier { .. }
                    | crate::fir::HeaderDeclarationKind::Constructor { .. }
                    | crate::fir::HeaderDeclarationKind::TypeAlias { .. } => None,
                })
                .and_then(|receiver| self.headers.syntax.ty(receiver))
                .is_some_and(|receiver| receiver.span == reference.span);
        if associated_receiver && reference.targs.is_empty() {
            if let Some((identity, _, expansion)) =
                self.signature_source_alias_expansion(scope, &reference.name)
            {
                if let Some(classifier) = expansion.non_null().obj_internal() {
                    self.record_type_spelling_binding(
                        selected_site,
                        super::super::type_spellings::ResolvedTypeSpelling {
                            classifier_segment: None,
                            alias: Some(identity),
                            resolved: None,
                        },
                    );
                    return Some(Ty::obj_name(classifier));
                }
            }
        }
        let lexical_classifier =
            self.lexically_nested_classifier_at(scope, &reference.name, include_scope_owner_body);
        let (qualified_binding, _) = self.qualified_type_scoped_classifier_binding(
            scope,
            &reference.name,
            include_scope_owner_body,
        );
        // Classifier before leaf, matching `Checker::type_ref_ty`: a declared classifier outranks a
        // builtin spelling of the same name. The binding carries the alias declaration and written
        // package/classifier boundary selected by this same scope-tower operation.
        let binding = lexical_classifier
            .map(crate::symbol_resolver::CompletedClassifierPath::ordinary_root)
            .or(qualified_binding)
            .or_else(|| {
                self.classifier_header_scope_owner(scope.owner)
                    .and_then(|owner| {
                        self.classifier_header_inherited_classifier(scope, owner, &reference.name)
                    })
                    .map(crate::symbol_resolver::CompletedClassifierPath::ordinary_root)
            });
        if let Some(binding) = binding {
            self.record_type_spelling_binding(
                selected_site,
                super::super::type_spellings::ResolvedTypeSpelling {
                    classifier_segment: (!reference.qualifier_arguments.is_empty())
                        .then_some(binding.classifier_segment),
                    alias: binding.selected.alias.as_ref().map(|alias| alias.identity),
                    resolved: None,
                },
            );
            let without_package_arguments =
                super::super::package_qualifier_type_arguments::without_package_arguments(
                    reference,
                    binding.classifier_segment,
                );
            let reference = without_package_arguments.as_ref().unwrap_or(reference);
            if let Some(alias) = binding.selected.alias {
                let ty = self.apply_signature_source_alias(
                    scope,
                    lexical,
                    reference,
                    include_scope_owner_body,
                    alias,
                    selected_site,
                )?;
                return Some(if reference.nullable() && ty != Ty::Error {
                    Ty::nullable(ty)
                } else {
                    ty
                });
            }
            let internal = binding.selected.classifier;
            let access = self
                .with_resolver(scope, |resolver| {
                    resolver.inaccessible_classifier_access(internal)
                })
                .ok();
            crate::trace_compiler!(
                "resolve",
                "compact type access declaration={:?} source={:?} spelling={} classifier={} inaccessible={access:?}",
                scope.owner,
                scope.source,
                reference.name,
                internal,
            );
            if let Some(access) = access {
                self.record_classifier_access_diagnostic_at(
                    scope.owner,
                    scope.source,
                    reference.span,
                    internal,
                    super::super::inaccessible_classifier_message(&reference.name, access),
                );
            }
            // The provider-normalized declaration, whichever module or classpath entry supplies it:
            // its variances check the written projections and its formal bounds give each `*` its
            // readable upper bound, exactly as the body checker reads the same classifier.
            let declaration = self
                .with_resolver(scope, |resolver| resolver.classifier(internal))
                .ok()?;
            let type_param_variances = &declaration.type_param_variances;
            if let Some(argument) =
                reference
                    .targs
                    .iter()
                    .enumerate()
                    .find_map(|(index, argument)| {
                        let variance = type_param_variances.get(index).copied().unwrap_or_default();
                        matches!(
                            (
                                variance,
                                argument.in_projection(),
                                argument.out_projection()
                            ),
                            (crate::types::TypeVariance::Out, true, _)
                                | (crate::types::TypeVariance::In, _, true)
                        )
                        .then_some(argument)
                    })
            {
                let mut display = super::super::BoundedSourceDisplay::new(usize::MAX);
                use std::fmt::Write as _;
                display
                    .write_str(self.classifier_source_spelling(internal))
                    .expect("unbounded type display");
                display.write_str("<").expect("unbounded type display");
                super::super::write_source_type_list_with(
                    &mut display,
                    &reference.targs,
                    &mut super::super::write_source_type_leaf,
                )
                .expect("unbounded type display");
                display.write_str(">").expect("unbounded type display");
                let projection_prefix = if argument.in_projection() { 3 } else { 4 };
                self.record_source_diagnostic_at(
                    scope.owner,
                    scope.source,
                    Span::new(
                        argument.span.lo.saturating_sub(projection_prefix),
                        argument.span.hi,
                    ),
                    format!(
                        "projection conflicts with variance of the corresponding type parameter of '{}'. Remove the projection or replace it with '*'.",
                        display.finish(),
                    ),
                );
                return Some(Ty::Error);
            }
            let default_star_bound = Ty::nullable(Ty::obj_name(crate::types::wk::any()));
            let classifier = self.table.class_by_type_name(internal);
            let captured_count = classifier.map_or(0, |classifier| {
                classifier.captured_type_parameters.type_params.len()
            });
            let mut arguments = Vec::with_capacity(reference.targs.len() + captured_count);
            let mut parsed = Vec::with_capacity(reference.targs.len());
            for argument in &reference.targs {
                parsed.push(if argument.is_star_projection() {
                    None
                } else {
                    let resolved = self.signature_type_ref_at_with_site(
                        scope,
                        lexical,
                        argument,
                        include_scope_owner_body,
                        self.selected_type_child(selected_site, reference, argument),
                    )?;
                    Some(super::super::projected_typeref_argument(
                        argument,
                        resolved,
                        default_star_bound,
                    ))
                });
            }
            let bindings = super::super::projected_classifier_argument_bindings(
                declaration.type_params(),
                &parsed,
            );
            for (index, (syntax, resolved)) in reference.targs.iter().zip(parsed).enumerate() {
                let argument = match resolved {
                    Some(resolved) => resolved,
                    None => {
                        let upper_bound = declaration
                            .type_param_bounds()
                            .get(index)
                            .and_then(|bounds| bounds.first())
                            .copied()
                            .map(|bound| {
                                crate::symbol_resolver::ty_subst_keep_unbound(bound, &bindings)
                            })
                            .unwrap_or(default_star_bound);
                        super::super::projected_typeref_argument(syntax, Ty::Error, upper_bound)
                    }
                };
                if syntax.is_star_projection() {
                    self.record_type_spelling_projection(
                        self.selected_type_child(selected_site, reference, syntax),
                        argument,
                    );
                }
                arguments.push(argument);
            }
            // An unqualified inner/local type supplies only its own arguments, while a qualified
            // spelling (`Outer<A>.Inner<B>`) is flattened by the parser as own arguments followed
            // by explicit captures (`[B, A]`). Append only captured slots not already supplied by
            // that syntax; otherwise compact Pass-1 inference publishes `[B, A, OuterFormal]` and
            // disagrees with the ordinary checker in Pass 2.
            if let Some(classifier) = self.table.class_by_type_name(internal) {
                let explicit_captured =
                    arguments.len().saturating_sub(classifier.type_params.len());
                let applied_outer = classifier.inner_of.and_then(|outer_owner| {
                    self.implicit_receivers(scope)
                        .into_iter()
                        .find_map(|receiver| {
                            self.table.applied_hierarchy(receiver).into_iter().find_map(
                                |(owner, applied, _)| (owner == outer_owner).then_some(applied),
                            )
                        })
                });
                arguments.extend(
                    classifier
                        .captured_type_parameters
                        .type_params
                        .iter()
                        .enumerate()
                        .skip(explicit_captured)
                        .map(|(ordinal, parameter)| {
                            applied_outer
                                .and_then(|outer| outer.type_args().get(ordinal).copied())
                                .unwrap_or_else(|| {
                                    Ty::ty_param(
                                        parameter,
                                        classifier
                                            .captured_type_parameters
                                            .type_param_bounds
                                            .get(ordinal)
                                            .copied()
                                            .unwrap_or(default_star_bound),
                                    )
                                })
                        }),
                );
            }
            let base = Ty::obj_args_name(internal, &arguments);
            return Some(if reference.nullable() {
                Ty::nullable(base)
            } else {
                base
            });
        }
        // Function types (`(A) -> B`, including suspend/receiver/context shapes), builtin spellings
        // and primitive arrays. `typeref_leaf` is the same free function the checker uses; only the
        // component recursion differs, so a component this path cannot resolve refuses the whole
        // reference instead of silently publishing `Ty::Error`.
        let mut unresolved = false;
        let leaf = super::super::typeref_leaf(reference, &mut |component| match self
            .signature_type_ref_at_with_site(
                scope,
                lexical,
                component,
                include_scope_owner_body,
                self.selected_type_child(selected_site, reference, component),
            ) {
            Some(ty) => ty,
            None => {
                unresolved = true;
                Ty::Error
            }
        });
        let leaf = leaf.filter(|_| !unresolved)?;
        Some(if reference.nullable() {
            Ty::nullable(leaf)
        } else {
            leaf
        })
    }

    /// Apply the source `typealias` carried by the already-selected classifier binding. The alias
    /// declaration—not its expansion—owns arity and projection semantics; no spelling lookup is
    /// repeated here.
    fn apply_signature_source_alias(
        &self,
        scope: crate::fir::SignatureScope,
        lexical: &super::super::CheckerScope<'_>,
        reference: &TypeRef,
        include_scope_owner_body: bool,
        alias: crate::libraries::AliasExpansion,
        selected_site: Option<SelectedTypeSite>,
    ) -> Option<Ty> {
        let identity = alias.identity;
        let formals = alias.formals;
        let expansion = alias.expansion;
        crate::trace_compiler!(
            "signature",
            "compact typealias use spelling={} formals={formals:?} template={expansion:?}",
            reference.name,
        );
        if matches!(AliasArguments::of(reference), AliasArguments::StarProjected) {
            return Some(star_projected_alias_type(
                &formals,
                expansion,
                &mut |owner| {
                    self.with_resolver(scope, |resolver| resolver.classifier(owner))
                        .ok()
                },
            ));
        }
        if reference.is_class_literal_lhs() && !reference.targs.is_empty() {
            // The checker reports these arguments (`class_literal_type_arguments`) regardless of
            // whether their count fits. An exact-arity useless application still contributes its
            // written arguments to an inferred declaration signature (`Arr<Int>::class` publishes
            // `KClass<Array<Int>>`), even though the expression checker reports those arguments as
            // deprecated. A wrong-arity useless application has no applicable substitution, so it
            // retains the alias's projected type only to let the checker own the diagnostic.
            let useless_type = super::super::class_literal_type_arguments::useless_alias_type(
                &formals,
                expansion,
                &mut |owner| {
                    self.with_resolver(scope, |resolver| resolver.classifier(owner))
                        .ok()
                },
            );
            if formals.len() != reference.targs.len() {
                return Some(useless_type.unwrap_or(Ty::Error));
            }
            // A non-useless alias is still a successfully bound type expression. The body
            // checker owns CLASS_LITERAL_LHS_NOT_A_CLASS; publishing `Ty::Error` here would make
            // signature finalization reinterpret the already-selected alias as unresolved and
            // emit a second, false diagnostic. Apply the selected alias below so the compact
            // graph remains typed until the checker reports the class-literal error.
        }
        if formals.len() != reference.targs.len() {
            self.record_source_diagnostic_at(
                scope.owner,
                scope.source,
                reference.span,
                format!(
                    "wrong number of type arguments for type alias '{}': expected {}, found {}.",
                    reference.name,
                    formals.len(),
                    reference.targs.len()
                ),
            );
            return Some(Ty::Error);
        }
        if formals.is_empty() {
            return Some(expansion);
        }
        let fallback_star_bound = Ty::nullable(Ty::obj("kotlin/Any"));
        let arguments = reference
            .targs
            .iter()
            .map(|argument| {
                let resolved = if argument.is_star_projection() {
                    Ty::Error
                } else {
                    self.signature_type_ref_at_with_site(
                        scope,
                        lexical,
                        argument,
                        include_scope_owner_body,
                        self.selected_type_child(selected_site, reference, argument),
                    )?
                };
                let projected = super::super::projected_typeref_argument(
                    argument,
                    resolved,
                    fallback_star_bound,
                );
                if argument.is_star_projection() {
                    self.record_type_spelling_projection(
                        self.selected_type_child(selected_site, reference, argument),
                        projected,
                    );
                }
                Some(projected)
            })
            .collect::<Option<Vec<_>>>()?;
        if let Some(origin) = selected_site.and_then(|site| site.abbreviation_origin) {
            self.selected_types.record(
                origin,
                crate::fir::ResolvedTypeAbbreviation {
                    alias: identity,
                    arguments: arguments.clone().into_boxed_slice(),
                    argument_path: Box::new([]),
                },
            );
        }
        let bindings = formals
            .into_iter()
            .zip(arguments)
            .collect::<crate::symbol_resolver::GSigBinds>();
        Some(crate::types::ty_subst_alias_expansion(expansion, &bindings))
    }

    /// Resolve a source type-alias lookup spelling to its declaration-owned expansion. This is a
    /// scope operation, not semantic identity: callers immediately use the qualified alias record
    /// and the expanded `Ty`, and neither value survives signature-graph evaluation.
    pub(super) fn signature_source_alias_expansion(
        &self,
        scope: crate::fir::SignatureScope,
        spelling: &str,
    ) -> Option<(crate::types::TypeName, Vec<String>, Ty)> {
        // The classifier and optional alias come from one scope-tower operation. Looking up alias
        // declarations separately would let a lower star-imported alias attach itself to a winning
        // same-package class whenever both normalize to the same target classifier.
        self.with_resolver(scope, |resolver| {
            resolver
                .qualified_type_alias_expansion(spelling)
                .map(|alias| (alias.identity, alias.formals, alias.expansion))
        })
        .ok()
    }

    pub(super) fn qualified_classifier(
        &self,
        scope: crate::fir::SignatureScope,
        spelling: &str,
    ) -> Option<crate::types::TypeName> {
        self.qualified_classifier_binding(scope, spelling).0
    }

    pub(super) fn qualified_type_classifier(
        &self,
        scope: crate::fir::SignatureScope,
        spelling: &str,
    ) -> Option<crate::types::TypeName> {
        self.qualified_type_classifier_binding(scope, spelling)
            .0
            .map(|binding| binding.selected.classifier)
    }

    /// Resolve a classifier spelling and retain the segment where the committed namespace walk
    /// failed. Success and diagnostics deliberately share this operation so a failed compact
    /// signature cannot be rendered through the legacy module-wide `ClassNames` projection.
    pub(super) fn qualified_classifier_binding(
        &self,
        scope: crate::fir::SignatureScope,
        spelling: &str,
    ) -> (Option<crate::types::TypeName>, Option<String>) {
        let (selected, failed_segment) = self.qualified_scoped_classifier_binding(scope, spelling);
        (selected.map(|selected| selected.classifier), failed_segment)
    }

    /// [`Self::qualified_classifier_binding`] with the alias declaration that named the selection.
    /// Only the file/import rung can select through an alias; a lexical classifier names none.
    fn qualified_scoped_classifier_binding(
        &self,
        scope: crate::fir::SignatureScope,
        spelling: &str,
    ) -> (
        Option<crate::symbol_resolver::ScopedClassifier>,
        Option<String>,
    ) {
        let segments = spelling.split('.').collect::<Vec<_>>();
        let Some(&first) = segments.first() else {
            return (None, Some(spelling.to_string()));
        };
        let mut lexical_owners = Vec::new();
        let mut owner = self
            .headers
            .declarations
            .anchor(scope.owner)
            .and_then(|anchor| anchor.owner);
        while let Some(declaration) = owner {
            let Some(anchor) = self.headers.declarations.anchor(declaration) else {
                return (None, Some(first.to_string()));
            };
            if anchor.kind == crate::fir::DeclarationKind::Classifier {
                if let Some(classifier) = self.classifier_types.get(&declaration).copied() {
                    lexical_owners.push(classifier);
                }
            }
            owner = anchor.owner;
        }
        let header_scope = self.classifier_header_scope_owner(scope.owner).is_some()
            || self
                .headers
                .declarations
                .anchor(scope.owner)
                .is_some_and(|anchor| anchor.kind == crate::fir::DeclarationKind::Classifier);
        // The stable declaration chain is the lexical classifier rung for a body. An enum-entry
        // body has no nominal classifier type, so its nested declarations are found here and
        // precede file/import candidates. A class or constructor header has not entered that body:
        // `class MyClass : Base` must not see `interface Base` declared inside MyClass.
        let declaration_nested = (!header_scope)
            .then(|| self.lexically_nested_classifier_at(scope, first, true))
            .flatten();
        self.with_resolver(scope, |resolver| {
            let mut current = declaration_nested;
            // Expression qualification selects its nearest root before walking the suffix. A later
            // miss is final and must not reinterpret the source spelling through another rung.
            if current.is_none() {
                for &owner in &lexical_owners {
                    let candidate = owner
                        .existing_nested_child(first)
                        .unwrap_or_else(|| crate::types::type_name_nested_child(owner, first));
                    if resolver.classifier(candidate).is_some() {
                        current = Some(candidate);
                        break;
                    }
                }
            }
            let inherited = || {
                for &owner in &lexical_owners {
                    match resolver.nested_classifier(Ty::obj_name(owner), first) {
                        crate::symbol_resolver::CandidateSelection::Selected(classifier) => {
                            return crate::symbol_resolver::CandidateSelection::Selected(
                                classifier,
                            );
                        }
                        crate::symbol_resolver::CandidateSelection::Ambiguous => {
                            return crate::symbol_resolver::CandidateSelection::Ambiguous;
                        }
                        crate::symbol_resolver::CandidateSelection::None => {}
                    }
                }
                crate::symbol_resolver::CandidateSelection::None
            };
            if current.is_none() && !header_scope {
                match inherited() {
                    crate::symbol_resolver::CandidateSelection::Selected(classifier) => {
                        current = Some(classifier);
                    }
                    crate::symbol_resolver::CandidateSelection::Ambiguous => {
                        return Some((None, Some(first.to_string())));
                    }
                    crate::symbol_resolver::CandidateSelection::None => {}
                }
            }
            if current.is_none() {
                let (selection, failed_segment) =
                    resolver.qualified_scoped_classifier_binding_in_scope(spelling);
                match selection {
                    crate::symbol_resolver::CandidateSelection::Selected(classifier) => {
                        return Some((Some(classifier), None));
                    }
                    crate::symbol_resolver::CandidateSelection::Ambiguous => {
                        return Some((None, failed_segment));
                    }
                    crate::symbol_resolver::CandidateSelection::None => {
                        return Some((None, failed_segment));
                    }
                }
            }
            if current.is_none() && header_scope {
                match inherited() {
                    crate::symbol_resolver::CandidateSelection::Selected(classifier) => {
                        current = Some(classifier);
                    }
                    crate::symbol_resolver::CandidateSelection::Ambiguous => {
                        return Some((None, Some(first.to_string())));
                    }
                    crate::symbol_resolver::CandidateSelection::None => {}
                }
            }
            let mut current = match current {
                Some(classifier) => classifier,
                None => return Some((None, Some(first.to_string()))),
            };
            for segment in &segments[1..] {
                let candidate = current
                    .existing_nested_child(segment)
                    .unwrap_or_else(|| crate::types::type_name_nested_child(current, segment));
                if resolver.classifier(candidate).is_none() {
                    return Some((None, Some(segment.to_string())));
                }
                current = candidate;
            }
            Some((
                Some(crate::symbol_resolver::ScopedClassifier {
                    classifier: current,
                    alias: None,
                }),
                None,
            ))
        })
        .ok()
        .unwrap_or_else(|| (None, Some(first.to_string())))
    }

    /// Resolve a type path by considering only complete candidates at each scope-tower rung. This
    /// is candidate applicability before selection, not a retry after a root was selected: type
    /// syntax can therefore choose an explicit import when a nearer same-named classifier does not
    /// contain the written suffix.
    pub(super) fn qualified_type_scoped_classifier_binding(
        &self,
        scope: crate::fir::SignatureScope,
        spelling: &str,
        include_scope_owner_body: bool,
    ) -> (
        Option<crate::symbol_resolver::CompletedClassifierPath>,
        Option<crate::symbol_resolver::ClassifierMiss>,
    ) {
        let segments = spelling.split('.').collect::<Vec<_>>();
        let Some(&first) = segments.first() else {
            return (
                None,
                Some(crate::symbol_resolver::ClassifierMiss::Unresolved(
                    spelling.to_string(),
                )),
            );
        };
        let mut lexical_owners = Vec::new();
        let mut owner = self
            .headers
            .declarations
            .anchor(scope.owner)
            .and_then(|anchor| anchor.owner);
        while let Some(declaration) = owner {
            let Some(anchor) = self.headers.declarations.anchor(declaration) else {
                return (
                    None,
                    Some(crate::symbol_resolver::ClassifierMiss::Unresolved(
                        first.to_string(),
                    )),
                );
            };
            if anchor.kind == crate::fir::DeclarationKind::Classifier {
                if let Some(classifier) = self.classifier_types.get(&declaration).copied() {
                    lexical_owners.push(classifier);
                }
            }
            owner = anchor.owner;
        }
        let header_scope = self.classifier_header_scope_owner(scope.owner).is_some()
            || self
                .headers
                .declarations
                .anchor(scope.owner)
                .is_some_and(|anchor| anchor.kind == crate::fir::DeclarationKind::Classifier);
        let declaration_nested = (!header_scope)
            .then(|| self.lexically_nested_classifier_at(scope, first, include_scope_owner_body))
            .flatten();
        self.with_resolver(scope, |resolver| {
            let mut failure = None;
            let mut advance = |candidate| {
                let (selected, failed_segment) =
                    resolver.classifier_path_from_selected_root(candidate, &segments[1..]);
                // A nearer rung that binds the root owns the diagnostic when no complete path is
                // applicable. Lower rungs may still supply a complete candidate, but their misses
                // must not replace this candidate's exact failed suffix.
                if selected.is_none() && failure.is_none() {
                    failure = failed_segment;
                }
                selected.map(crate::symbol_resolver::CompletedClassifierPath::ordinary_root)
            };
            if let Some(candidate) = declaration_nested {
                if let Some(selected) = advance(candidate) {
                    return Some((Some(selected), None));
                }
            }
            for &owner in &lexical_owners {
                let candidate = owner
                    .existing_nested_child(first)
                    .unwrap_or_else(|| crate::types::type_name_nested_child(owner, first));
                if resolver.classifier(candidate).is_some() {
                    if let Some(selected) = advance(candidate) {
                        return Some((Some(selected), None));
                    }
                }
            }
            let inherited = || {
                for &owner in &lexical_owners {
                    match resolver.nested_classifier(Ty::obj_name(owner), first) {
                        crate::symbol_resolver::CandidateSelection::Selected(classifier) => {
                            let (selected, _) = resolver
                                .classifier_path_from_selected_root(classifier, &segments[1..]);
                            if let Some(classifier) = selected {
                                return crate::symbol_resolver::CandidateSelection::Selected(
                                    classifier,
                                );
                            }
                        }
                        crate::symbol_resolver::CandidateSelection::Ambiguous => {
                            return crate::symbol_resolver::CandidateSelection::Ambiguous;
                        }
                        crate::symbol_resolver::CandidateSelection::None => {}
                    }
                }
                crate::symbol_resolver::CandidateSelection::None
            };
            if !header_scope {
                match inherited() {
                    crate::symbol_resolver::CandidateSelection::Selected(classifier) => {
                        return Some((
                            Some(
                                crate::symbol_resolver::CompletedClassifierPath::ordinary_root(
                                    classifier,
                                ),
                            ),
                            None,
                        ));
                    }
                    crate::symbol_resolver::CandidateSelection::Ambiguous => {
                        return Some((
                            None,
                            Some(crate::symbol_resolver::ClassifierMiss::Unresolved(
                                first.to_string(),
                            )),
                        ));
                    }
                    crate::symbol_resolver::CandidateSelection::None => {}
                }
            }
            let (selection, failed_segment) =
                resolver.qualified_type_classifier_binding_in_scope(spelling);
            match selection {
                crate::symbol_resolver::CandidateSelectionWithTies::Selected(path) => {
                    return Some((Some(path), None));
                }
                crate::symbol_resolver::CandidateSelectionWithTies::Ambiguous(candidates) => {
                    return Some((
                        None,
                        Some(crate::symbol_resolver::ClassifierMiss::Ambiguous(
                            candidates
                                .into_iter()
                                .map(|candidate| candidate.selected)
                                .collect(),
                        )),
                    ));
                }
                crate::symbol_resolver::CandidateSelectionWithTies::None => {
                    if failure.is_none() && failed_segment.is_some() {
                        failure = failed_segment;
                    }
                }
            }
            if header_scope {
                match inherited() {
                    crate::symbol_resolver::CandidateSelection::Selected(classifier) => {
                        return Some((
                            Some(
                                crate::symbol_resolver::CompletedClassifierPath::ordinary_root(
                                    classifier,
                                ),
                            ),
                            None,
                        ));
                    }
                    crate::symbol_resolver::CandidateSelection::Ambiguous => {
                        return Some((
                            None,
                            Some(crate::symbol_resolver::ClassifierMiss::Unresolved(
                                first.to_string(),
                            )),
                        ));
                    }
                    crate::symbol_resolver::CandidateSelection::None => {}
                }
            }
            Some((
                None,
                Some(crate::symbol_resolver::ClassifierMiss::Unresolved(
                    failure.unwrap_or_else(|| first.to_string()),
                )),
            ))
        })
        .ok()
        .unwrap_or_else(|| {
            (
                None,
                Some(crate::symbol_resolver::ClassifierMiss::Unresolved(
                    first.to_string(),
                )),
            )
        })
    }

    pub(super) fn qualified_type_classifier_binding(
        &self,
        scope: crate::fir::SignatureScope,
        spelling: &str,
    ) -> (
        Option<crate::symbol_resolver::CompletedClassifierPath>,
        Option<crate::symbol_resolver::ClassifierMiss>,
    ) {
        self.qualified_type_scoped_classifier_binding(scope, spelling, true)
    }

    pub(super) fn qualified_classifier_or_source_alias(
        &self,
        scope: crate::fir::SignatureScope,
        spelling: &str,
    ) -> Option<crate::types::TypeName> {
        self.qualified_classifier(scope, spelling).or_else(|| {
            self.applied_source_alias_expansion(scope, spelling, &[])
                .and_then(|alias| alias.expansion.non_null().obj_internal())
        })
    }

    pub(super) fn qualified_receiver_ty(
        &self,
        scope: crate::fir::SignatureScope,
        qualifier: &str,
        origin: crate::fir::OriginId,
        demand: &mut dyn FnMut(
            crate::fir::DeclarationId,
        )
            -> Result<crate::fir::ResolvedSignature, crate::fir::DiagnosticId>,
    ) -> Result<crate::fir::ResolvedTy, crate::fir::DiagnosticId> {
        // A CLASSIFIER prefix is a namespace, not a value: `A.E.OK` reads the entry `OK` of the
        // nested enum `A.E`, and only the last classifier in the path denotes a value at all. Take
        // the LONGEST classifier prefix first, then read the remaining segments as ordinary members.
        let segments = qualifier.split('.').collect::<Vec<_>>();
        for length in (1..=segments.len()).rev() {
            let prefix = segments[..length].join(".");
            let receiver = if length == 1 {
                <Self as crate::fir::SignatureSemantics>::select_value(
                    self, scope, &prefix, origin, None, demand,
                )
            } else {
                self.qualified_classifier(scope, &prefix)
                    .and_then(|classifier| self.classifier_value_type(classifier))
                    .and_then(|ty| crate::fir::ResolvedTy::new(ty).ok())
                    .ok_or_else(Self::failure)
            };
            let Ok(mut receiver) = receiver else {
                continue;
            };
            let mut resolved = true;
            let mut pending = segments[length..].iter();
            if let Some(segment) = pending.next() {
                if let Some(entries) =
                    self.prioritized_enum_entries_on_qualifier(scope, &prefix, segment)
                {
                    match crate::fir::ResolvedTy::new(entries) {
                        Ok(next) => receiver = next,
                        Err(_) => resolved = false,
                    }
                } else {
                    match <Self as crate::fir::SignatureSemantics>::select_member(
                        self, scope, segment, origin, receiver, None, demand,
                    ) {
                        Ok(next) => receiver = next,
                        Err(_) => resolved = false,
                    }
                }
            }
            if resolved {
                for segment in pending {
                    match <Self as crate::fir::SignatureSemantics>::select_member(
                        self, scope, segment, origin, receiver, None, demand,
                    ) {
                        Ok(next) => receiver = next,
                        Err(_) => {
                            resolved = false;
                            break;
                        }
                    }
                }
            }
            if resolved {
                return Ok(receiver);
            }
        }
        // A class qualifier can expose an associated property without itself being a runtime value:
        // `System.out.println(...)` binds `System` as the classifier namespace, `out` through the
        // normal provider-backed property selector, and only then has a `PrintStream` value on
        // which `println` is selected. Do not fabricate a singleton/companion receiver for an
        // ordinary Java classifier.
        for length in (1..segments.len()).rev() {
            let prefix = segments[..length].join(".");
            let Some(classifier) = self.qualified_classifier(scope, &prefix) else {
                continue;
            };
            let Some(mut receiver) = self
                .with_resolver(scope, |resolver| {
                    resolver.associated_property(classifier, segments[length])
                })
                .ok()
                .map(|property| property.ty)
            else {
                continue;
            };
            let mut resolved = true;
            for segment in &segments[length + 1..] {
                match <Self as crate::fir::SignatureSemantics>::select_member(
                    self,
                    scope,
                    segment,
                    origin,
                    crate::fir::ResolvedTy::new(receiver).map_err(|_| Self::failure())?,
                    None,
                    demand,
                ) {
                    Ok(next) => receiver = next.get(),
                    Err(_) => {
                        resolved = false;
                        break;
                    }
                }
            }
            if resolved {
                return crate::fir::ResolvedTy::new(receiver).map_err(|_| Self::failure());
            }
        }
        Err(Self::failure())
    }

    /// Runtime value denoted by a classifier name. Plain classes have no value; objects denote
    /// their singleton, classes with companions denote the companion, and enum classifiers denote
    /// the receiver against which entries and synthetic enum members are selected.
    pub(super) fn classifier_value_type(&self, classifier: crate::types::TypeName) -> Option<Ty> {
        if self.classifier_is_singleton(classifier) {
            return Some(Ty::obj_name(classifier));
        }
        self.classifier_companion(classifier)
            .or_else(|| self.classifier_is_enum(classifier).then_some(classifier))
            .map(Ty::obj_name)
    }

    /// The companion object `classifier` declares, from its source or library declaration.
    pub(super) fn classifier_companion(
        &self,
        classifier: crate::types::TypeName,
    ) -> Option<crate::types::TypeName> {
        self.table
            .classes
            .get(&classifier)
            .and_then(|declaration| declaration.companion_internal)
            .or_else(|| {
                self.table
                    .libraries
                    .classifier(classifier)
                    .and_then(|declaration| {
                        declaration
                            .companion_object
                            .as_ref()
                            .map(|(_, companion)| *companion)
                    })
            })
    }
}

impl ProductionSignatureSemantics<'_> {
    /// Run `f` over the signature-stage view a member-extension candidate is collected and scored
    /// in: the declaration's own receiver tower (`dispatch_receiver`), context arguments matched
    /// against that same tower, and applicability judged from `argument_kinds`. No `Checker` is
    /// constructed — the fabricated scope the old shim built carried nothing but this list.
    /// Candidate collection, inference, argument mapping and overload selection remain the shared
    /// checker algorithms.
    fn with_signature_member_extension_probes<R>(
        &self,
        scope: crate::fir::SignatureScope,
        dispatch_receiver: Ty,
        argument_kinds: &[crate::symbol_resolver::CallArgKind],
        f: impl FnOnce(
            &dyn crate::symbol_source::SymbolSource,
            &dyn crate::assignable::TypeOracle,
            &[super::super::ImplicitReceiver],
            super::super::MemberExtensionProbes<'_>,
        ) -> R,
    ) -> R {
        let module = crate::module_symbols::ModuleSymbols::for_file(self.table, scope.source.raw());
        let source = crate::symbol_source::CompositeSource::new(vec![
            &module as &dyn crate::symbol_source::SymbolSource,
            &*self.table.libraries as &dyn crate::symbol_source::SymbolSource,
        ]);
        let oracle = crate::symbol_resolver::SourceOracle(&source);
        let receivers = [super::super::ImplicitReceiver::signature_receiver(
            dispatch_receiver,
        )];
        // Dispatch selection stays on exactly this receiver rung, while context arguments may
        // come from any value in the declaration's nearest-first receiver tower. In particular,
        // a member declaration's context receiver is distinct from its class dispatch receiver.
        let context_receivers = self.implicit_receivers(scope);
        let explicit_context_arguments = self
            .headers
            .scopes
            .file(scope.source)
            .is_some_and(|file| file.explicit_context_arguments);
        let deferred_type_variables = self.active_postponed_type_variables(scope);
        f(
            &source,
            &oracle,
            &receivers,
            super::super::MemberExtensionProbes {
                explicit_context_arguments,
                deferred_type_variables: &deferred_type_variables,
                select_context_arguments: &|parameters| {
                    super::super::context_argument_types(&context_receivers, parameters, &oracle)
                        .map(|types| {
                            types
                                .into_iter()
                                .map(|ty| {
                                    (
                                    super::super::ResolvedContextArgument::ImplicitReceiver(
                                        super::super::ImplicitReceiverSelection::signature_receiver(
                                            ty,
                                        ),
                                    ),
                                    ty,
                                )
                                })
                                .collect()
                        })
                        .ok_or(super::super::MissingContextParameter {
                            index: 0,
                            ty: Ty::Error,
                        })
                },
                is_spread_arg: &|argument| argument_kinds[argument].is_spread(),
                unit_coerced_lambda: &|_, _, _| None,
                score_candidate: &|params, call_sig, slots| {
                    // Applicability is the shared per-argument rule: each argument must fit the
                    // parameter it maps to (a vararg's element unless the argument is spread).
                    let parameter_by_argument = super::super::call_argument_parameter_indices(
                        slots.args.len(),
                        params.len(),
                        slots.arg_names,
                        slots.trailing_lambda,
                        call_sig,
                    )?;
                    for (argument, parameter) in parameter_by_argument.into_iter().enumerate() {
                        let kind = &argument_kinds[argument];
                        let declared = *params.get(parameter)?;
                        let expected =
                            if call_sig.vararg_index == Some(parameter) && !kind.is_spread() {
                                declared.array_read_elem().unwrap_or(declared)
                            } else {
                                declared
                            };
                        if !kind.fits_parameter(&*self.table.libraries, &source, expected) {
                            return None;
                        }
                    }
                    let mapped = super::super::map_call_sig_args_with_trailing(
                        slots.args,
                        slots.arg_names,
                        params.len(),
                        call_sig,
                        slots.trailing_lambda,
                    )
                    .ok()?;
                    Some(super::super::CallCandidateScore {
                        rank: (
                            0,
                            std::cmp::Reverse(mapped.iter().filter(|slot| slot.is_none()).count()),
                            !call_sig.vararg,
                        ),
                        sam_signatures: Vec::new(),
                    })
                },
            },
        )
    }

    /// Member-extension selection for signature evaluation, through
    /// [`Self::with_signature_member_extension_probes`].
    pub(super) fn signature_member_extension_call(
        &self,
        scope: crate::fir::SignatureScope,
        extension_receiver: Ty,
        dispatch_receiver: Ty,
        name: &str,
        call: SignatureMemberExtensionArguments<'_>,
        result_constraint: super::super::CallResultConstraint,
        selection: super::super::MemberExtensionSelection,
    ) -> SignatureMemberExtensionCallSelection {
        let SignatureMemberExtensionArguments {
            arguments: argument_kinds,
            names,
            explicit_type_arguments,
            trailing_lambda,
        } = call;
        // The shared selector uses expression IDs only to preserve source argument mapping. Compact
        // signature evaluation has no AST IDs, so supply dense transient ordinals; no callback below
        // dereferences them and none escape this call.
        let arguments = argument_kinds
            .iter()
            .map(crate::symbol_resolver::CallArgKind::ty)
            .collect::<Vec<_>>();
        let argument_ordinals = (0..arguments.len())
            .map(|ordinal| crate::ast::ExprId(ordinal as u32))
            .collect::<Vec<_>>();
        let selected = self.with_signature_member_extension_probes(
            scope,
            dispatch_receiver,
            argument_kinds,
            |source, oracle, receivers, probes| {
                super::super::member_extension_function_with(
                    source,
                    oracle,
                    receivers,
                    probes,
                    super::super::MemberExtensionFunctionCall {
                        extension_receiver,
                        result_constraint,
                        name,
                        args: &argument_ordinals,
                        arg_tys: &arguments,
                        arg_names: names,
                        explicit_type_args: explicit_type_arguments,
                        trailing_lambda,
                        inference_bindings: None,
                    },
                    selection,
                )
            },
        );
        crate::trace_compiler!(
            "signature",
            "member extension call name={name} extension={extension_receiver:?} dispatch={dispatch_receiver:?} arguments={arguments:?} result={}",
            match &selected {
                super::super::MemberExtensionFunctionSelection::Selected(_) => "selected",
                super::super::MemberExtensionFunctionSelection::None(_) => "none",
                super::super::MemberExtensionFunctionSelection::Ambiguous(_) => "ambiguous",
            },
        );
        match selected {
            super::super::MemberExtensionFunctionSelection::Selected(selected) => {
                SignatureMemberExtensionCallSelection::Selected {
                    result: selected.ret,
                    declaration: selected.stable_declaration,
                }
            }
            super::super::MemberExtensionFunctionSelection::None(rejections) => {
                SignatureMemberExtensionCallSelection::None(rejections.convention_candidates)
            }
            super::super::MemberExtensionFunctionSelection::Ambiguous(candidates) => {
                SignatureMemberExtensionCallSelection::Ambiguous(candidates)
            }
        }
    }

    /// Postponed-argument expectations for `receiver.spelling(arguments)` from the member
    /// extensions the declaration's dispatch receivers contribute, or `None` when none of them
    /// takes the call. The lambda shape is the one Pass-2 checking gives the same call
    /// ([`super::super::member_extension_selection::member_extension_lambda_shape_with`]), so a formal only a lambda can solve
    /// stays open in the expectation exactly as it does for a top-level callee.
    pub(super) fn member_extension_call_expectations(
        &self,
        receiver: Ty,
        call: super::semantics::ReceiverLevelCall<'_, '_>,
    ) -> Option<Box<[Option<crate::fir::ResolvedTy>]>> {
        let super::semantics::ReceiverLevelCall {
            scope,
            spelling,
            arguments,
            type_arguments,
            trailing_lambda,
            result_constraint,
            refresh_typed_postponed,
        } = call;
        let argument_kinds = arguments
            .iter()
            .map(Self::probe_argument_kind)
            .collect::<Vec<_>>();
        // A postponed argument is what the shape is for; every other argument is evidence.
        let partial_arguments = arguments
            .iter()
            .zip(&argument_kinds)
            .map(|(argument, kind)| match argument {
                crate::fir::SigCallArgumentProbe::Typed(_) => Some(kind.ty()),
                crate::fir::SigCallArgumentProbe::PostponedLambda { .. }
                | crate::fir::SigCallArgumentProbe::PostponedCallableReference { .. } => None,
            })
            .collect::<Vec<_>>();
        let names = arguments
            .iter()
            .map(|argument| match argument {
                crate::fir::SigCallArgumentProbe::Typed(argument) => {
                    argument.name.map(str::to_owned)
                }
                crate::fir::SigCallArgumentProbe::PostponedLambda { name, .. }
                | crate::fir::SigCallArgumentProbe::PostponedCallableReference { name, .. } => {
                    name.map(str::to_owned)
                }
            })
            .collect::<Vec<_>>();
        let names = names
            .iter()
            .any(Option::is_some)
            .then_some(names.as_slice());
        // Dense transient ordinals, as in `signature_member_extension_call`.
        let argument_ordinals = (0..arguments.len())
            .map(|ordinal| crate::ast::ExprId(ordinal as u32))
            .collect::<Vec<_>>();
        let mut shape = None;
        for dispatch_receiver in self.signature_dispatch_receivers(scope) {
            match self.with_signature_member_extension_probes(
                scope,
                dispatch_receiver,
                &argument_kinds,
                |source, oracle, receivers, probes| {
                    super::super::member_extension_selection::member_extension_lambda_shape_with(
                        source,
                        oracle,
                        receivers,
                        probes,
                        result_constraint,
                        refresh_typed_postponed,
                        super::super::MemberExtensionCall {
                            extension_receiver: receiver,
                            name: spelling,
                            args: &argument_ordinals,
                            partial_arg_tys: &partial_arguments,
                            arg_names: names,
                            explicit_type_args: type_arguments,
                            trailing_lambda,
                        },
                    )
                },
            ) {
                super::super::member_extension_selection::MemberExtensionLambdaShape::Selected(
                    selected,
                ) => {
                    shape = Some(selected);
                    break;
                }
                super::super::member_extension_selection::MemberExtensionLambdaShape::Ambiguous => {
                    return Some(vec![None; arguments.len()].into_boxed_slice());
                }
                super::super::member_extension_selection::MemberExtensionLambdaShape::Absent => {}
            }
        }
        let shape = shape?;
        crate::trace_compiler!(
            "signature",
            "member extension expectations receiver={receiver:?} spelling={spelling} parameters={:?}",
            shape.argument_parameters,
        );
        let slots = (0..arguments.len()).map(Some).collect::<Vec<_>>();
        Some(Self::postponed_expectations(
            arguments,
            &slots,
            &shape.argument_parameters,
            refresh_typed_postponed,
        ))
    }

    /// Postponed-argument expectations at one qualified receiver rung: ordinary members first,
    /// then the member extensions contributed by the declaration's dispatch receivers.
    pub(super) fn qualified_receiver_call_expectations(
        &self,
        receiver: Ty,
        call: super::semantics::ReceiverLevelCall<'_, '_>,
    ) -> Result<Option<Box<[Option<crate::fir::ResolvedTy>]>>, crate::fir::DiagnosticId> {
        if let Some((parameters, slots)) =
            self.receiver_member_level_expectations(receiver, call)?
        {
            crate::trace_compiler!(
                "signature",
                "member call expectations receiver={receiver:?} spelling={} parameters={parameters:?} slots={slots:?}",
                call.spelling,
            );
            return Ok(Some(Self::postponed_expectations(
                call.arguments,
                &slots,
                &parameters,
                call.refresh_typed_postponed,
            )));
        }
        Ok(self.member_extension_call_expectations(receiver, call))
    }

    /// Lambda expectations for a bare call that resolves on the MEMBER level of one of the
    /// implicit receivers — its member functions and the constructors of a same-named inner
    /// classifier (`class C : A { fun f() = B(arg) }` constructs `A.B` exactly as `this.B(arg)`) —
    /// mirroring the top-level selection above one receiver rung at a time.
    pub(super) fn bare_member_call_expectations(
        &self,
        scope: crate::fir::SignatureScope,
        spelling: &str,
        arguments: &[crate::fir::SigCallArgumentProbe<'_>],
        type_arguments: &[Ty],
        trailing_lambda: bool,
        refresh_typed_postponed: bool,
        expected: Option<Ty>,
        demand: &mut dyn FnMut(
            crate::fir::DeclarationId,
        )
            -> Result<crate::fir::ResolvedSignature, crate::fir::DiagnosticId>,
    ) -> Result<Box<[Option<crate::fir::ResolvedTy>]>, crate::fir::DiagnosticId> {
        let call = super::semantics::ReceiverLevelCall {
            scope,
            spelling,
            arguments,
            type_arguments,
            trailing_lambda,
            refresh_typed_postponed,
            result_constraint: super::super::CallResultConstraint::direct(expected),
        };
        for receiver in self
            .implicit_receivers(scope)
            .into_iter()
            .chain(self.enclosing_lexical_singleton_receivers(scope))
        {
            if let Some((parameters, slots)) =
                self.receiver_member_level_expectations(receiver, call)?
            {
                return Ok(Self::postponed_expectations(
                    arguments,
                    &slots,
                    &parameters,
                    refresh_typed_postponed,
                ));
            }
            // At each receiver rung, callable-valued properties follow ordinary functions before
            // the tower advances outward. Reuse the property selector and invoke expectation
            // operation used by final call selection so an explicit or lazily inferred member
            // function type can contextualize its arguments without duplicating member lookup.
            if let Some(callee) =
                self.selected_member_property_type(scope, receiver, spelling, demand)?
            {
                if let Ok(expectations) = self.invoke_argument_expectations_for_refresh(
                    scope,
                    callee,
                    arguments,
                    refresh_typed_postponed,
                ) {
                    return Ok(expectations);
                }
            }
            // Then the member extensions an enclosing declaration contributes for this receiver,
            // as final selection's member-extension rung does.
            if let Some(expectations) = self.member_extension_call_expectations(receiver, call) {
                return Ok(expectations);
            }
        }
        Err(Self::failure())
    }

    /// Singleton receivers contributed by every LEXICALLY enclosing classifier: an enclosing
    /// `object` itself, or a class's companion. A nested declaration may use their members by bare
    /// name even though there is no captured outer instance, so they do not appear on the ordinary
    /// implicit-receiver chain.
    ///
    /// Enclosure is decided by SOURCE SPAN CONTAINMENT, not by the stable anchor's `owner`: a nested
    /// classifier is inventoried without an owner, so the anchor chain stops at the nested class and
    /// never reaches the outer one.
    pub(super) fn enclosing_lexical_singleton_receivers(
        &self,
        scope: crate::fir::SignatureScope,
    ) -> Vec<Ty> {
        let mut receivers = Vec::new();
        for owner in self.lexical_class_names(scope) {
            if self.classifier_is_singleton(owner) {
                receivers.push(Ty::obj_name(owner));
            }
            // The enclosing class's own companion, then those of everything it INHERITS: a subclass
            // reaches its superclass's companion members by bare name.
            for candidate in std::iter::once(owner).chain(
                self.table
                    .applied_hierarchy(Ty::obj_name(owner))
                    .into_iter()
                    .map(|(inherited, _, _)| inherited),
            ) {
                // A dependency superclass publishes its companion through its classifier record,
                // exactly as a source one does through its signature.
                let companion = match self.table.class_by_type_name(candidate) {
                    Some(signature) => signature.companion_internal,
                    None => self
                        .table
                        .libraries
                        .classifier(candidate)
                        .and_then(|declaration| {
                            declaration.companion_object.as_ref().map(|(_, c)| *c)
                        }),
                };
                if let Some(companion) = companion {
                    let ty = Ty::obj_name(companion);
                    if !receivers.contains(&ty) {
                        receivers.push(ty);
                    }
                }
            }
        }
        receivers
    }

    /// Dispatch receivers visible to member-extension selection at this signature site. Runtime
    /// implicit receivers keep their scope-tower priority; lexically enclosing companions follow
    /// because their members are in scope without an enclosing instance.
    pub(super) fn signature_dispatch_receivers(
        &self,
        scope: crate::fir::SignatureScope,
    ) -> Vec<Ty> {
        let mut entries = self.signature_implicit_receivers(scope);
        for singleton in self.enclosing_lexical_singleton_receivers(scope) {
            if !entries.iter().any(|receiver| receiver.ty == singleton) {
                entries.push(SignatureImplicitReceiver::ordinary(singleton));
            }
        }
        // Match checked member-extension selection: every ordinary dispatch receiver precedes
        // context-parameter receivers, including lexical singletons/companions; each group's
        // nearest-first order remains stable.
        entries.sort_by_key(|receiver| u8::from(receiver.context_parameter));
        entries.into_iter().map(|receiver| receiver.ty).collect()
    }

    /// Classifier stub positions in one source. Built once: range containment is a per-declaration
    /// question, and scanning every classifier in the module for it is quadratic.
    fn classifier_stub_positions(&self, source: crate::fir::SourceFileId) -> Vec<usize> {
        let missing = self.classifier_stubs_by_source.borrow().is_none();
        if missing {
            let mut by_source = Vec::new();
            for (position, stub) in self.headers.stubs.iter().enumerate() {
                if stub.kind != crate::fir::DeclarationKind::Classifier {
                    continue;
                }
                let index = stub.source.raw() as usize;
                if by_source.len() <= index {
                    by_source.resize(index + 1, Vec::new());
                }
                by_source[index].push(position);
            }
            *self.classifier_stubs_by_source.borrow_mut() = Some(by_source);
        }
        self.classifier_stubs_by_source
            .borrow()
            .as_ref()
            .and_then(|by_source| by_source.get(source.raw() as usize))
            .cloned()
            .unwrap_or_default()
    }

    /// Internal names of the classifiers lexically enclosing this scope's declaration, innermost
    /// first. Decided by SOURCE SPAN CONTAINMENT — a nested classifier is inventoried without an
    /// anchor `owner`, so the stable chain stops before reaching the outer class.
    pub(super) fn lexical_class_names(
        &self,
        scope: crate::fir::SignatureScope,
    ) -> Vec<crate::types::TypeName> {
        let Some(anchor) = self.headers.declarations.anchor(scope.owner) else {
            return Vec::new();
        };
        // Stable declaration ownership is the authoritative lexical chain. A bounded classifier
        // header's source range need not cover its body, so range containment alone loses the outer
        // enum/class while evaluating a companion member. Keep the containment inventory below only
        // for local/nested classifiers whose compact anchor intentionally has no owner.
        let mut result = Vec::new();
        let mut push_classifier_chain = |classifier: crate::types::TypeName| {
            let mut current = Some(classifier);
            while let Some(candidate) = current {
                if self.table.classes.contains_key(&candidate) && !result.contains(&candidate) {
                    result.push(candidate);
                }
                current = candidate.nested_owner();
            }
        };
        let mut current = Some(scope.owner);
        while let Some(declaration) = current {
            let Some(current_anchor) = self.headers.declarations.anchor(declaration) else {
                break;
            };
            if current_anchor.kind == crate::fir::DeclarationKind::Classifier {
                if let Some(classifier) = self.classifier_types.get(&declaration).copied() {
                    push_classifier_chain(classifier);
                }
            }
            current = current_anchor.owner;
        }
        let mut enclosing = self
            .classifier_stub_positions(scope.source)
            .into_iter()
            .filter_map(|position| {
                let stub = &self.headers.stubs[position];
                (stub.range.lo <= anchor.range.lo && anchor.range.hi <= stub.range.hi).then(
                    || {
                        self.classifier_types
                            .get(&stub.id)
                            .copied()
                            .map(|classifier| (stub.range.hi - stub.range.lo, classifier))
                    },
                )?
            })
            .collect::<Vec<_>>();
        // Innermost first, so lexical scope-tower priority remains source-correct.
        enclosing.sort_by_key(|(width, _)| *width);
        for (_, classifier) in enclosing {
            push_classifier_chain(classifier);
        }
        result
    }

    /// Classifier namespaces callable from one lexical classifier rung. Kotlin declarations keep
    /// their classifier namespace closed; a foreign provider may explicitly permit inherited
    /// classifier callables, in which case its supertype namespace follows the lexical owner.
    pub(super) fn lexical_classifier_callable_owners(
        &self,
        scope: crate::fir::SignatureScope,
    ) -> Vec<crate::types::TypeName> {
        let mut owners = Vec::new();
        for lexical in self.lexical_class_names(scope) {
            for (owner, _, _) in self.table.applied_hierarchy(Ty::obj_name(lexical)) {
                if (owner == lexical || self.table.libraries.inherits_classifier_callables(owner))
                    && !owners.contains(&owner)
                {
                    owners.push(owner);
                }
            }
        }
        owners
    }

    /// The source spelling that names `classifier` in a label or lexical lookup. A local
    /// classifier's identity is opaque, so its spelling comes from its stable declaration.
    pub(super) fn classifier_source_spelling(&self, classifier: crate::types::TypeName) -> &str {
        self.table
            .classes
            .get(&classifier)
            .and_then(|class| class.stable_declaration)
            .and_then(|declaration| self.headers.stub(declaration))
            .and_then(|stub| self.headers.source_simple_name(stub))
            .unwrap_or_else(|| classifier.nested_segment_ref())
    }

    pub(super) fn bound_classifier_identity(
        &self,
        declaration: crate::fir::DeclarationId,
    ) -> crate::types::TypeName {
        *self
            .classifier_types
            .get(&declaration)
            .expect("a bound local classifier call must retain its semantic identity")
    }

    fn lexically_nested_classifier_at(
        &self,
        scope: crate::fir::SignatureScope,
        spelling: &str,
        include_scope_owner_body: bool,
    ) -> Option<crate::types::TypeName> {
        if spelling.contains('.') {
            return None;
        }
        // Prefer the stable declaration ownership chain. In particular, an enum-entry body is an
        // anonymous classifier scope which has no nominal `TypeName`, while a nested class declared
        // there does have a stable classifier declaration (`A$X$Inner`). Source-span containment
        // alone can only see the surrounding enum `A` and would manufacture the wrong child.
        let scope_owner_classifier = self
            .headers
            .declarations
            .anchor(scope.owner)
            .filter(|anchor| anchor.kind == crate::fir::DeclarationKind::Classifier)
            .and_then(|_| self.classifier_types.get(&scope.owner).copied());
        let mut owner = if include_scope_owner_body {
            Some(scope.owner)
        } else {
            self.headers.declarations.anchor(scope.owner)?.owner
        };
        let owned_classifier = |declaration| {
            self.headers.owned_stubs(declaration).find_map(|stub| {
                let classifier = self.classifier_types.get(&stub.id).copied()?;
                (stub.kind == crate::fir::DeclarationKind::Classifier
                    && self.headers.source_simple_name(stub) == Some(spelling))
                .then_some(classifier)
            })
        };
        while let Some(declaration) = owner {
            if let Some(classifier) = owned_classifier(declaration) {
                return Some(classifier);
            }
            // The companion's static classifier scope follows its owner's. A companion whose own
            // header is being resolved has not entered that scope.
            let companion = self.headers.owned_stubs(declaration).find(|stub| {
                stub.kind == crate::fir::DeclarationKind::Classifier
                    && stub.flags.has(crate::fir::DeclarationFlags::COMPANION)
            });
            if let Some(companion) = companion
                .filter(|companion| include_scope_owner_body || companion.id != scope.owner)
            {
                if let Some(classifier) = owned_classifier(companion.id) {
                    return Some(classifier);
                }
            }
            owner = self.headers.declarations.anchor(declaration)?.owner;
        }
        let nested_class = |owner: crate::types::TypeName| {
            let candidate = owner
                .existing_nested_child(spelling)
                .unwrap_or_else(|| crate::types::type_name_nested_child(owner, spelling));
            self.table
                .classes
                .contains_key(&candidate)
                .then_some(candidate)
        };
        self.lexical_class_names(scope)
            .into_iter()
            .filter(|owner| include_scope_owner_body || Some(*owner) != scope_owner_classifier)
            .find_map(|owner| {
                nested_class(owner).or_else(|| {
                    let companion = self.table.classes.get(&owner)?.companion_internal?;
                    (include_scope_owner_body || Some(companion) != scope_owner_classifier)
                        .then(|| nested_class(companion))
                        .flatten()
                })
            })
    }

    /// Inherited nested classifiers of the classifier whose own header is being resolved. This is
    /// deliberately consulted only after lexical/package/import lookup: Kotlin accepts a package
    /// `Category` in `class Child(x: Category) : Left, Right` even when both supertypes declare a
    /// nested `Category`, while the inherited declaration remains visible when no nearer binding
    /// exists.
    fn classifier_header_inherited_classifier(
        &self,
        scope: crate::fir::SignatureScope,
        header_owner: crate::fir::DeclarationId,
        spelling: &str,
    ) -> Option<crate::types::TypeName> {
        if spelling.contains('.') {
            return None;
        }
        let owner = self.classifier_types.get(&header_owner).copied()?;
        let module = crate::module_symbols::ModuleSymbols::for_file(self.table, scope.source.raw());
        let source = crate::symbol_source::CompositeSource::new(vec![
            &module as &dyn crate::symbol_source::SymbolSource,
            &*self.table.libraries as &dyn crate::symbol_source::SymbolSource,
        ]);
        let roots = crate::symbol_resolver::direct_supertypes(&source, Ty::obj_name(owner))
            .into_iter()
            .filter_map(Ty::obj_internal)
            .collect::<Vec<_>>();
        match crate::symbol_resolver::inherited_nested_classifier_name(
            spelling,
            roots,
            |candidate| {
                crate::symbol_resolver::direct_supertypes(&source, Ty::obj_name(candidate))
                    .into_iter()
                    .filter_map(Ty::obj_internal)
                    .collect()
            },
            |candidate| {
                crate::symbol_resolver::inherited_classifier_shape(&source, candidate, owner)
                    .is_some()
            },
        ) {
            crate::symbol_resolver::InheritedNestedClassifier::Found(classifier) => {
                Some(classifier)
            }
            crate::symbol_resolver::InheritedNestedClassifier::NotFound
            | crate::symbol_resolver::InheritedNestedClassifier::Ambiguous => None,
        }
    }

    pub(super) fn member_extension_property_for(
        &self,
        scope: crate::fir::SignatureScope,
        extension_receiver: Ty,
        dispatch_receiver: Ty,
        name: &str,
    ) -> Result<Option<(Ty, Option<crate::fir::DeclarationId>)>, ()> {
        let module = crate::module_symbols::ModuleSymbols::for_file(self.table, scope.source.raw());
        let source = crate::symbol_source::CompositeSource::new(vec![
            &module as &dyn crate::symbol_source::SymbolSource,
            &*self.table.libraries as &dyn crate::symbol_source::SymbolSource,
        ]);
        let oracle = crate::symbol_resolver::SourceOracle(&source);
        let receivers = [super::super::ImplicitReceiver::signature_receiver(
            dispatch_receiver,
        )];
        let selected = super::super::member_extension_property(
            &source,
            &oracle,
            &receivers,
            &|parameters| {
                super::super::context_argument_types(&[dispatch_receiver], parameters, &oracle).map(
                    |types| {
                        types
                            .into_iter()
                            .map(|ty| {
                                (
                                    super::super::ResolvedContextArgument::ImplicitReceiver(
                                        super::super::ImplicitReceiverSelection::signature_receiver(
                                            ty,
                                        ),
                                    ),
                                    ty,
                                )
                            })
                            .collect()
                    },
                )
            },
            extension_receiver,
            name,
            crate::symbol_resolver::PropertyAccess::Read,
        )?;
        Ok(selected.map(|selected| (selected.ty, selected.stable_declaration)))
    }
}

pub(super) enum SignatureMemberExtensionCallSelection {
    Selected {
        result: Ty,
        declaration: Option<crate::fir::DeclarationId>,
    },
    None(
        Vec<super::super::member_extension_selection::MemberExtensionConventionDiagnosticCandidate>,
    ),
    Ambiguous(Vec<super::super::MemberExtensionFunctionCandidate>),
}

#[derive(Clone, Copy, Default)]
pub(super) struct SignatureMemberExtensionArguments<'a> {
    pub(super) arguments: &'a [crate::symbol_resolver::CallArgKind],
    pub(super) names: Option<&'a [Option<String>]>,
    pub(super) explicit_type_arguments: &'a [Ty],
    pub(super) trailing_lambda: bool,
}
