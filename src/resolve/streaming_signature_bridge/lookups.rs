//! Resolver-backed classifier, type, and member-extension lookups for signature solving.

use super::*;

impl ProductionSignatureSemantics<'_> {
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

    /// Fold a dotted qualifier into the receiver type its last segment is read against.
    ///
    /// The first segment is an ordinary value/classifier lookup (an object denotes itself, a class
    /// with a companion denotes the companion, an enum denotes itself); every later segment is an
    /// ordinary member read. This is the same walk the checker performs for a qualified expression —
    /// the graph contributes no lookup rules of its own.
    /// The classifier a DOTTED spelling names, walking nested children from an in-scope root:
    /// `Container.Nested` is `Container$Nested`. Every intermediate segment must itself exist as a
    /// classifier, so a value-qualified spelling falls through to receiver folding untouched.
    /// A type reference resolved WITHOUT a body checker: a type parameter of the owning declaration
    /// chain, or a classifier reachable through this file's import scope, with its type arguments
    /// resolved the same way. Returns `None` for the forms that still need the checker so the caller
    /// can fall back rather than publish a wrong type.
    pub(super) fn signature_type_ref(
        &self,
        scope: crate::fir::SignatureScope,
        lexical: &super::super::CheckerScope<'_>,
        reference: &TypeRef,
    ) -> Option<Ty> {
        self.signature_type_ref_at(scope, lexical, reference, true)
    }

    /// Locate the first declaration type component whose compact lookup fails. The packed header
    /// owns every component span, so Pass 1 can diagnose `Outer<Missing>` at `Missing` without
    /// retaining source text or blaming the otherwise valid outer classifier.
    pub(super) fn unresolved_signature_type_ref<'a>(
        &self,
        scope: crate::fir::SignatureScope,
        lexical: &super::super::CheckerScope<'_>,
        reference: &'a TypeRef,
    ) -> &'a TypeRef {
        let nested = reference
            .fun_params
            .iter()
            .chain(reference.arg.iter().map(|argument| &**argument))
            .chain(reference.targs.iter());
        for component in nested {
            if self
                .signature_type_ref_at(scope, lexical, component, true)
                .is_none()
            {
                return self.unresolved_signature_type_ref(scope, lexical, component);
            }
        }
        reference
    }

    /// Resolve a classifier header type in the lexical scope immediately outside the classifier
    /// body. The classifier's own type parameters remain in `lexical`, but nested declarations from
    /// its body are not visible in its supertype list (`class C : Base { interface Base }`).
    pub(super) fn classifier_header_type_ref(
        &self,
        scope: crate::fir::SignatureScope,
        lexical: &super::super::CheckerScope<'_>,
        reference: &TypeRef,
    ) -> Option<Ty> {
        self.signature_type_ref_at(scope, lexical, reference, false)
    }

    fn signature_type_ref_at(
        &self,
        scope: crate::fir::SignatureScope,
        lexical: &super::super::CheckerScope<'_>,
        reference: &TypeRef,
        include_scope_owner_body: bool,
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
        let associated_receiver = self
            .headers
            .stubs
            .iter()
            .find(|stub| stub.id == scope.owner)
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
            if let Some((_, expansion)) =
                self.signature_source_alias_expansion(scope, &reference.name)
            {
                if let Some(classifier) = expansion.non_null().obj_internal() {
                    return Some(Ty::obj_name(classifier));
                }
            }
        }
        let lexical_classifier =
            self.lexically_nested_classifier_at(scope, &reference.name, include_scope_owner_body);
        // An alias is a declaration in the classifier namespace, but lexical nested classifiers
        // occupy a nearer scope-tower rung than imports. Only consult alias expansion when that
        // lexical rung did not answer; otherwise `Outer.Box` can accidentally inherit an imported
        // `typealias Box = ...` target in the finalized Pass-1 signature.
        if lexical_classifier.is_none() {
            if let Some(ty) =
                self.signature_source_alias(scope, lexical, reference, include_scope_owner_body)
            {
                return Some(if reference.nullable() && ty != Ty::Error {
                    Ty::nullable(ty)
                } else {
                    ty
                });
            }
        }
        // Classifier before leaf, matching `Checker::type_ref_ty`: a declared classifier outranks a
        // builtin spelling of the same name.
        if let Some(internal) =
            lexical_classifier.or_else(|| self.qualified_classifier(scope, &reference.name))
        {
            let fallback_star_bound = Ty::nullable(Ty::obj("kotlin/Any"));
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
                    let resolved = self.signature_type_ref_at(
                        scope,
                        lexical,
                        argument,
                        include_scope_owner_body,
                    )?;
                    Some(super::super::projected_typeref_argument(
                        argument,
                        resolved,
                        fallback_star_bound,
                    ))
                });
            }
            let bindings =
                classifier.map_or_else(crate::symbol_resolver::GSigBinds::new, |classifier| {
                    super::super::projected_classifier_argument_bindings(
                        &classifier.type_params,
                        &parsed,
                    )
                });
            for (index, (syntax, resolved)) in reference.targs.iter().zip(parsed).enumerate() {
                arguments.push(match resolved {
                    Some(resolved) => resolved,
                    None => {
                        let upper_bound = classifier
                            .and_then(|classifier| classifier.type_param_bounds.get(index))
                            .copied()
                            .map(|bound| {
                                crate::symbol_resolver::ty_subst_keep_unbound(bound, &bindings)
                            })
                            .unwrap_or(fallback_star_bound);
                        super::super::projected_typeref_argument(syntax, Ty::Error, upper_bound)
                    }
                });
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
                                            .unwrap_or(fallback_star_bound),
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
            .signature_type_ref_at(scope, lexical, component, include_scope_owner_body)
        {
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

    /// A source `typealias` reachable from this file, expanded with the exact use-site arguments.
    /// This operation precedes target-classifier lookup because the alias declaration—not its
    /// expansion—owns arity and projection semantics.
    fn signature_source_alias(
        &self,
        scope: crate::fir::SignatureScope,
        lexical: &super::super::CheckerScope<'_>,
        reference: &TypeRef,
        include_scope_owner_body: bool,
    ) -> Option<Ty> {
        let (formals, expansion) = self.signature_source_alias_expansion(scope, &reference.name)?;
        crate::trace_compiler!(
            "signature",
            "compact typealias use spelling={} formals={formals:?} template={expansion:?}",
            reference.name,
        );
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
                    self.signature_type_ref_at(scope, lexical, argument, include_scope_owner_body)?
                };
                Some(super::super::projected_typeref_argument(
                    argument,
                    resolved,
                    fallback_star_bound,
                ))
            })
            .collect::<Option<Vec<_>>>()?;
        let bindings = formals
            .into_iter()
            .zip(arguments)
            .collect::<crate::symbol_resolver::GSigBinds>();
        Some(crate::symbol_resolver::ty_subst(expansion, &bindings))
    }

    /// Resolve a source type-alias lookup spelling to its declaration-owned expansion. This is a
    /// scope operation, not semantic identity: callers immediately use the qualified alias record
    /// and the expanded `Ty`, and neither value survives signature-graph evaluation.
    pub(super) fn signature_source_alias_expansion(
        &self,
        scope: crate::fir::SignatureScope,
        spelling: &str,
    ) -> Option<(Vec<String>, Ty)> {
        if spelling.contains('.') || spelling.contains('/') {
            return None;
        }
        let imports = self.function_import_scope(scope.source).ok()?;
        let expansion = |identity: crate::types::TypeName| {
            self.table
                .source_alias_expansions
                .get(&identity)
                .cloned()
                .or_else(|| {
                    self.table
                        .libraries
                        .type_alias_expansion(identity)
                        .map(|alias| (alias.formals, alias.expansion))
                })
        };
        let identity = if let Some((owner, declared_name)) = imports.explicit_target(spelling) {
            owner
                .existing_classifier(&declared_name)
                .filter(|&id| expansion(id).is_some())?
        } else {
            let mut found = None;
            for level in imports.levels() {
                for &package in level {
                    let Some(candidate) = crate::types::existing_type_name_child(package, spelling)
                        .filter(|&id| expansion(id).is_some())
                    else {
                        continue;
                    };
                    match found {
                        None => found = Some(candidate),
                        // Two distinct alias declarations in one level: ambiguous, kotlinc rejects.
                        Some(previous) if previous == candidate => {}
                        Some(_) => return None,
                    }
                }
                if found.is_some() {
                    break;
                }
            }
            found?
        };
        expansion(identity)
    }

    pub(super) fn qualified_classifier(
        &self,
        scope: crate::fir::SignatureScope,
        spelling: &str,
    ) -> Option<crate::types::TypeName> {
        self.qualified_classifier_binding(scope, spelling).0
    }

    /// Resolve a classifier spelling and retain the segment where the committed namespace walk
    /// failed. Success and diagnostics deliberately share this operation so a failed compact
    /// signature cannot be rendered through the legacy module-wide `ClassNames` projection.
    pub(super) fn qualified_classifier_binding(
        &self,
        scope: crate::fir::SignatureScope,
        spelling: &str,
    ) -> (Option<crate::types::TypeName>, Option<String>) {
        let mut segments = spelling.split('.');
        let Some(first) = segments.next() else {
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
        self.with_resolver(scope, |resolver| {
            let mut current = None;
            for owner in lexical_owners {
                match resolver.nested_classifier(Ty::obj_name(owner), first) {
                    crate::symbol_resolver::CandidateSelection::Selected(classifier) => {
                        current = Some(classifier);
                        break;
                    }
                    crate::symbol_resolver::CandidateSelection::Ambiguous => {
                        return Some((None, Some(first.to_string())));
                    }
                    crate::symbol_resolver::CandidateSelection::None => {}
                }
            }
            let mut current = match current {
                Some(classifier) => classifier,
                None => {
                    let (selection, failed_segment) =
                        resolver.qualified_classifier_binding_in_scope(spelling);
                    return Some(match selection {
                        crate::symbol_resolver::CandidateSelection::Selected(classifier) => {
                            (Some(classifier), None)
                        }
                        crate::symbol_resolver::CandidateSelection::None
                        | crate::symbol_resolver::CandidateSelection::Ambiguous => {
                            (None, failed_segment)
                        }
                    });
                }
            };
            for segment in segments {
                let candidate = current
                    .existing_nested_child(segment)
                    .unwrap_or_else(|| crate::types::type_name_nested_child(current, segment));
                if resolver.classifier(candidate).is_none() {
                    return Some((None, Some(segment.to_string())));
                }
                current = candidate;
            }
            Some((Some(current), None))
        })
        .ok()
        .unwrap_or_else(|| (None, Some(first.to_string())))
    }

    pub(super) fn qualified_classifier_or_source_alias(
        &self,
        scope: crate::fir::SignatureScope,
        spelling: &str,
    ) -> Option<crate::types::TypeName> {
        self.qualified_classifier(scope, spelling).or_else(|| {
            self.applied_source_alias_expansion(scope, spelling, &[])
                .and_then(|(_, expansion)| expansion.non_null().obj_internal())
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
            for segment in &segments[length..] {
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
                    resolver.classifier_associated_property(classifier, segments[length])
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
        let companion = self
            .table
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
            });
        companion
            .or_else(|| self.classifier_is_enum(classifier).then_some(classifier))
            .map(Ty::obj_name)
    }
}

impl ProductionSignatureSemantics<'_> {
    /// Member-extension selection for signature evaluation: the declaration's own receiver tower,
    /// and context arguments matched against that same tower. No `Checker` is constructed — the
    /// fabricated scope the old shim built carried nothing but this list. Candidate collection,
    /// inference, argument mapping and overload selection remain the shared checker algorithms.
    pub(super) fn signature_member_extension_call(
        &self,
        scope: crate::fir::SignatureScope,
        extension_receiver: Ty,
        dispatch_receiver: Ty,
        name: &str,
        call: SignatureMemberExtensionArguments<'_>,
        expected_result: Option<Ty>,
        selection: super::super::MemberExtensionSelection,
    ) -> Option<(Ty, Option<crate::fir::DeclarationId>)> {
        let SignatureMemberExtensionArguments {
            types: arguments,
            names,
            spread,
            explicit_type_arguments,
            trailing_lambda,
        } = call;
        let module = crate::module_symbols::ModuleSymbols::for_file(self.table, scope.source.raw());
        let source = crate::symbol_source::CompositeSource::new(vec![
            &module as &dyn crate::symbol_source::SymbolSource,
            &*self.table.libraries as &dyn crate::symbol_source::SymbolSource,
        ]);
        let oracle = crate::symbol_resolver::SourceOracle(&source);
        let receivers = [super::super::ImplicitReceiver::signature_receiver(
            dispatch_receiver,
        )];
        let explicit_context_arguments = self
            .headers
            .scopes
            .file(scope.source)
            .is_some_and(|file| file.explicit_context_arguments);
        // The shared selector uses expression IDs only to preserve source argument mapping. Compact
        // signature evaluation has no AST IDs, so supply dense transient ordinals; no callback below
        // dereferences them and none escape this call.
        let argument_ordinals = (0..arguments.len())
            .map(|ordinal| crate::ast::ExprId(ordinal as u32))
            .collect::<Vec<_>>();
        let selected = super::super::member_extension_function_with(
            &source,
            &oracle,
            &receivers,
            explicit_context_arguments,
            &|parameters| {
                super::super::context_argument_types(&[dispatch_receiver], parameters, &oracle)
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
            &|argument| spread.get(argument).copied().unwrap_or(false),
            &|_, _, _| None,
            &|params, call_sig, slots| {
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
            super::super::MemberExtensionFunctionCall {
                extension_receiver,
                expected_result,
                name,
                args: &argument_ordinals,
                arg_tys: arguments,
                arg_names: names,
                explicit_type_args: explicit_type_arguments,
                trailing_lambda,
            },
            selection,
        );
        crate::trace_compiler!(
            "signature",
            "member extension call name={name} extension={extension_receiver:?} dispatch={dispatch_receiver:?} arguments={arguments:?} result={}",
            match &selected {
                Ok(Some(_)) => "selected",
                Ok(None) => "none",
                Err(()) => "ambiguous",
            },
        );
        let selected = selected.ok()??;
        Some((selected.ret, selected.stable_declaration))
    }

    /// Lambda expectations for a bare call that resolves to a MEMBER of one of the implicit
    /// receivers, mirroring the top-level selection above one receiver rung at a time.
    pub(super) fn bare_member_call_expectations(
        &self,
        scope: crate::fir::SignatureScope,
        spelling: &str,
        arguments: &[crate::fir::SigCallArgumentProbe<'_>],
        type_arguments: &[Ty],
        trailing_lambda: bool,
        demand: &mut dyn FnMut(
            crate::fir::DeclarationId,
        )
            -> Result<crate::fir::ResolvedSignature, crate::fir::DiagnosticId>,
    ) -> Result<Box<[Option<crate::fir::ResolvedTy>]>, crate::fir::DiagnosticId> {
        for receiver in self
            .implicit_receivers(scope)
            .into_iter()
            .chain(self.enclosing_lexical_singleton_receivers(scope))
        {
            let selected = self.with_resolver(scope, |resolver| {
                let (mut functions, properties) =
                    resolver.receiver_callables(receiver, spelling).into_parts();
                functions.overloads = self
                    .implicit_context_candidates(scope, std::mem::take(&mut functions.overloads));
                let callables = crate::libraries::Callables::from_parts(functions, properties);
                let (kinds, slots) =
                    Self::probe_call_arguments(callables.functions(), arguments, trailing_lambda)?;
                let projected =
                    self.project_postponed_callables(scope, receiver, callables, &kinds);
                let selected_parameters = match resolver.select_receiver_function_with_params(
                    receiver,
                    spelling,
                    projected.arguments(),
                    type_arguments,
                    projected.callables(),
                ) {
                    Some((_, parameters)) => parameters,
                    None => self.common_postponed_parameters(
                        resolver,
                        arguments,
                        resolver.receiver_function_parameter_shapes(
                            receiver,
                            projected.arguments(),
                            type_arguments,
                            projected.callables(),
                        ),
                    )?,
                };
                let parameters = selected_parameters
                    .into_iter()
                    .map(|parameter| {
                        resolver
                            .functional_expectation(parameter)
                            .unwrap_or(parameter)
                    })
                    .collect::<Vec<_>>();
                crate::trace_compiler!(
                    "signature",
                    "member call expectation {spelling} receiver={receiver:?} parameters={parameters:?}",
                );
                Some((parameters, slots))
            });
            if let Ok((parameters, slots)) = selected {
                return Ok(Self::postponed_expectations(arguments, &slots, &parameters));
            }
            // At each receiver rung, callable-valued properties follow ordinary functions before
            // the tower advances outward. Reuse the property selector and invoke expectation
            // operation used by final call selection so an explicit or lazily inferred member
            // function type can contextualize its arguments without duplicating member lookup.
            if let Some(callee) =
                self.selected_member_property_type(scope, receiver, spelling, demand)?
            {
                if let Ok(expectations) =
                    <Self as crate::fir::SignatureSemantics>::invoke_argument_expectations(
                        self, scope, callee, arguments,
                    )
                {
                    return Ok(expectations);
                }
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
                if let Some(companion) = self
                    .table
                    .class_by_type_name(candidate)
                    .and_then(|signature| signature.companion_internal)
                {
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
        let mut receivers = self.implicit_receivers(scope);
        for singleton in self.enclosing_lexical_singleton_receivers(scope) {
            if !receivers.contains(&singleton) {
                receivers.push(singleton);
            }
        }
        receivers
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
            .headers
            .stubs
            .iter()
            .filter(|stub| {
                stub.source == scope.source
                    && stub.kind == crate::fir::DeclarationKind::Classifier
                    && stub.range.lo <= anchor.range.lo
                    && anchor.range.hi <= stub.range.hi
            })
            .filter_map(|stub| {
                self.classifier_types
                    .get(&stub.id)
                    .copied()
                    .map(|classifier| (stub.range.hi - stub.range.lo, classifier))
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

    /// A classifier named by BARE spelling from inside an enclosing class: `class Outer { class
    /// Nested; fun test() = Nested() }`. Import-scope lookup only sees top-level and imported names,
    /// so a lexically nested sibling had no way to resolve.
    pub(super) fn lexically_nested_classifier(
        &self,
        scope: crate::fir::SignatureScope,
        spelling: &str,
    ) -> Option<crate::types::TypeName> {
        self.lexically_nested_classifier_at(scope, spelling, true)
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
        while let Some(declaration) = owner {
            if let Some(classifier) = self.headers.stubs.iter().find_map(|stub| {
                let anchor = self.headers.declarations.anchor(stub.id)?;
                let classifier = self.classifier_types.get(&stub.id).copied()?;
                (stub.kind == crate::fir::DeclarationKind::Classifier
                    && anchor.owner == Some(declaration)
                    && classifier.nested_segment_ref() == spelling)
                    .then_some(classifier)
            }) {
                return Some(classifier);
            }
            owner = self.headers.declarations.anchor(declaration)?.owner;
        }
        self.lexical_class_names(scope)
            .into_iter()
            .filter(|owner| include_scope_owner_body || Some(*owner) != scope_owner_classifier)
            .find_map(|owner| {
                let candidate = owner
                    .existing_nested_child(spelling)
                    .unwrap_or_else(|| crate::types::type_name_nested_child(owner, spelling));
                self.table
                    .classes
                    .contains_key(&candidate)
                    .then_some(candidate)
            })
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
        )?;
        Ok(selected.map(|selected| (selected.ty, selected.stable_declaration)))
    }
}

#[derive(Clone, Copy, Default)]
pub(super) struct SignatureMemberExtensionArguments<'a> {
    pub(super) types: &'a [Ty],
    pub(super) names: Option<&'a [Option<String>]>,
    pub(super) spread: &'a [bool],
    pub(super) explicit_type_arguments: &'a [Ty],
    pub(super) trailing_lambda: bool,
}
