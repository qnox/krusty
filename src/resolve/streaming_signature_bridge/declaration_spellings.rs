//! Stable metadata spelling publication from compact declaration headers.
//!
//! Source type aliases may be expanded by the parser before semantic resolution. The compact
//! header arena owns both the expanded type and its as-written counterpart by `HeaderTypeId`, so
//! production signature collection never reopens `File::alias_spellings` or walks declarations by
//! parser coordinates.

use super::super::{spelling_of_ref_with, spelling_scope, TParams};
use crate::fir::{
    DeclarationId, DeclarationKind, HeaderDeclarationKind, HeaderTypeBoundRange, HeaderTypeId,
    HeaderTypeParameterRange, StreamedHeaderModule,
};
use crate::spelling::{DeclaredSpellings, RecordedTypeAnnotations, Spelled};

/// The module-wide inputs every declared-type spelling reads: source alias expansion templates and
/// each file's recorded type-use annotations.
struct SpellingContext {
    expansions:
        crate::fx_hash::HashMap<crate::types::TypeName, (Spelled, Vec<String>, crate::types::Ty)>,
    annotations: crate::fx_hash::HashMap<crate::fir::SourceFileId, RecordedTypeAnnotations>,
}

/// Expand a statement-local alias's written argument spellings through the stable structural
/// template captured at its declaration. Local aliases have no metadata identity of their own, so
/// publication records only the expanded shape. Repeated formal occurrences deliberately clone
/// one already-bound source argument spelling; diagnostics still walk the source argument once.
fn project_local_alias_spelling(
    semantics: &super::ProductionSignatureSemantics<'_>,
    context: &SpellingContext,
    projection: &super::bound_local_alias_templates::AppliedLocalTypeSpellingProjection,
    written: Spelled,
) -> Option<Spelled> {
    use super::bound_local_alias_templates::{
        LocalTypeSpellingOccurrence, LocalTypeSpellingProjection,
    };

    fn overlay(
        occurrence: &LocalTypeSpellingOccurrence,
        context: &SpellingContext,
        mut spelling: Spelled,
    ) -> Option<Spelled> {
        let annotations = context.annotations.get(&occurrence.source_file)?;
        let inherited = annotations.at(occurrence.source.span.lo).iter().cloned();
        let expanded = if spelling.alias.is_some() {
            &mut spelling.expansion_annotations
        } else {
            &mut spelling.annotations
        };
        *expanded = inherited.chain(std::mem::take(expanded)).collect();
        spelling.definitely_non_null |= occurrence.source.definitely_non_null();
        if let Some(name) = annotations.parameter_name_at(occurrence.source.span.lo) {
            spelling.parameter_name = Some(name.into());
        }
        Some(spelling)
    }

    fn projected(
        semantics: &super::ProductionSignatureSemantics<'_>,
        context: &SpellingContext,
        projection: &LocalTypeSpellingProjection,
        source_arguments: &[(crate::types::Ty, Spelled)],
        bindings: &crate::symbol_resolver::GSigBinds,
    ) -> Option<(crate::types::Ty, Spelled)> {
        match projection {
            LocalTypeSpellingProjection::Formal {
                ordinal,
                occurrence,
            } => {
                let (_, spelling) = source_arguments.get(*ordinal as usize)?.clone();
                let resolved = crate::symbol_resolver::ty_subst_keep_unbound(
                    occurrence.binding.resolved?,
                    bindings,
                );
                Some((resolved, overlay(occurrence, context, spelling)?))
            }
            LocalTypeSpellingProjection::Overlay { occurrence, inner } => {
                let (_, spelling) =
                    projected(semantics, context, inner, source_arguments, bindings)?;
                let resolved = crate::symbol_resolver::ty_subst_keep_unbound(
                    occurrence.binding.resolved?,
                    bindings,
                );
                Some((resolved, overlay(occurrence, context, spelling)?))
            }
            LocalTypeSpellingProjection::Structural {
                occurrence,
                children,
            } => {
                let mut children = children
                    .iter()
                    .map(|child| projected(semantics, context, child, source_arguments, bindings))
                    .collect::<Option<Vec<_>>>()?;
                let annotations = context.annotations.get(&occurrence.source_file)?;
                let source = occurrence.binding.classifier_segment.and_then(|segment| {
                    super::super::package_qualifier_type_arguments::without_package_arguments(
                        &occurrence.source,
                        segment,
                    )
                });
                let source = source.as_ref().unwrap_or(&occurrence.source);
                if !source.fun_params.is_empty() || source.name == "<fun>" {
                    if source.fun_suspend() {
                        children.truncate(source.fun_params.len());
                    }
                    let spelling = Spelled {
                        classifier_segment: occurrence.binding.classifier_segment,
                        definitely_non_null: source.definitely_non_null(),
                        args: children.into_iter().map(|(_, spelling)| spelling).collect(),
                        annotations: annotations.at(source.span.lo).to_vec(),
                        parameter_name: annotations
                            .parameter_name_at(source.span.lo)
                            .map(Into::into),
                        ..Spelled::default()
                    };
                    let resolved = crate::symbol_resolver::ty_subst_keep_unbound(
                        occurrence.binding.resolved?,
                        bindings,
                    );
                    return Some((resolved, spelling));
                }
                children.truncate(source.targs.len());
                let alias = occurrence.binding.alias;
                let alias_args = alias
                    .is_some()
                    .then(|| children.clone())
                    .unwrap_or_default();
                let dependency_alias = alias
                    .and_then(|identity| semantics.table.libraries.type_alias_expansion(identity));
                let template = alias.and_then(|identity| {
                    context
                        .expansions
                        .get(&identity)
                        .map(|(rhs, formals, expansion)| (rhs, formals.as_slice(), *expansion))
                        .or_else(|| {
                            dependency_alias.as_ref().map(|classpath| {
                                (
                                    &classpath.expansion_spelling,
                                    classpath.formals.as_slice(),
                                    classpath.expansion,
                                )
                            })
                        })
                });
                let expansion_annotations = template
                    .map(|(rhs, _, _)| {
                        rhs.expansion_annotations
                            .iter()
                            .chain(&rhs.annotations)
                            .cloned()
                            .collect()
                    })
                    .unwrap_or_default();
                let spelling = Spelled {
                    classifier_segment: occurrence.binding.classifier_segment,
                    definitely_non_null: source.definitely_non_null(),
                    use_site_definitely_non_null: source.definitely_non_null(),
                    alias,
                    alias_args,
                    args: if alias.is_some() {
                        super::super::type_spellings::expansion_arg_spellings(template, &children)
                    } else {
                        children.into_iter().map(|(_, spelling)| spelling).collect()
                    },
                    annotations: annotations.at(source.span.lo).to_vec(),
                    expansion_annotations,
                    unsafe_variance: template.is_some_and(|(rhs, _, _)| rhs.unsafe_variance),
                    parameter_name: annotations
                        .parameter_name_at(source.span.lo)
                        .map(Into::into),
                    flexible_upper_bound: None,
                    enhanced_nullability: false,
                };
                let resolved = crate::symbol_resolver::ty_subst_keep_unbound(
                    occurrence.binding.resolved?,
                    bindings,
                );
                Some((resolved, spelling))
            }
        }
    }

    let mut written = written;
    let source_arguments = projection
        .arguments
        .iter()
        .copied()
        .zip(std::mem::take(&mut written.args))
        .collect::<Vec<_>>();
    let bindings = projection
        .formals
        .iter()
        .cloned()
        .zip(projection.arguments.iter().copied())
        .collect::<crate::symbol_resolver::GSigBinds>();
    let (_, mut spelling) = projected(
        semantics,
        context,
        &projection.root,
        &source_arguments,
        &bindings,
    )?;
    spelling.definitely_non_null |= written.definitely_non_null;
    spelling.unsafe_variance |= written.unsafe_variance;
    spelling.annotations = written
        .annotations
        .into_iter()
        .chain(spelling.annotations)
        .collect();
    spelling.expansion_annotations = written
        .expansion_annotations
        .into_iter()
        .chain(spelling.expansion_annotations)
        .collect();
    if written.parameter_name.is_some() {
        spelling.parameter_name = written.parameter_name;
    }
    Some(spelling)
}

fn type_parameter_names(
    headers: &StreamedHeaderModule,
    range: HeaderTypeParameterRange,
) -> Option<Vec<String>> {
    headers
        .syntax
        .type_parameters(range)
        .iter()
        .map(|parameter| headers.lookup_names.get(parameter.name).map(str::to_owned))
        .collect()
}

fn classifier_scope_names(
    headers: &StreamedHeaderModule,
    declaration: DeclarationId,
) -> Option<Vec<String>> {
    let mut owner = Some(declaration);
    while let Some(candidate) = owner {
        if let Some(header) = headers.syntax.declaration(candidate) {
            if let HeaderDeclarationKind::Classifier {
                type_parameters,
                lexical_type_parameter_captures,
                ..
            } = header.kind
            {
                let mut names = type_parameter_names(headers, lexical_type_parameter_captures)?;
                names.extend(type_parameter_names(headers, type_parameters)?);
                return Some(names);
            }
        }
        owner = headers
            .declarations
            .anchor(candidate)
            .and_then(|anchor| anchor.owner);
    }
    Some(Vec::new())
}

fn declaration_scope(
    headers: &StreamedHeaderModule,
    declaration: DeclarationId,
    own: HeaderTypeParameterRange,
) -> Option<(Vec<String>, TParams)> {
    let anchor = headers.declarations.anchor(declaration)?;
    let mut names = if anchor.kind == DeclarationKind::Classifier {
        match headers.syntax.declaration(declaration)?.kind {
            HeaderDeclarationKind::Classifier {
                lexical_type_parameter_captures,
                ..
            } => type_parameter_names(headers, lexical_type_parameter_captures)?,
            _ => return None,
        }
    } else {
        match anchor.owner {
            Some(owner) => classifier_scope_names(headers, owner)?,
            None => Vec::new(),
        }
    };
    let own = type_parameter_names(headers, own)?;
    names.extend(own.iter().cloned());
    let scope = spelling_scope(&names);
    Some((own, scope))
}

fn spelling(
    headers: &StreamedHeaderModule,
    semantics: &super::ProductionSignatureSemantics<'_>,
    scope_owner: DeclarationId,
    ty: HeaderTypeId,
    scope: &TParams,
    context: &SpellingContext,
) -> Option<Spelled> {
    let materialized = headers
        .syntax
        .transient_written_type_ref(ty, &headers.lookup_names)?;
    let structural_ids = headers
        .syntax
        .resolved_written_spelling_preorder(ty, &mut |syntax| {
            semantics
                .selected_types
                .spelling(syntax)
                .map(|spelling| spelling.classifier_segment)
        })?;
    let signature_scope = crate::fir::SignatureScope {
        owner: scope_owner,
        source: headers.declarations.anchor(scope_owner)?.source,
    };
    let no_source_substitutions = crate::fx_hash::HashMap::default();
    let spellings = crate::spelling::SourceSpellings {
        aliases: &no_source_substitutions,
        annotations: context.annotations.get(&signature_scope.source)?,
    };
    let mut structural_ids = structural_ids.into_iter();
    let resolved = std::cell::RefCell::new(crate::fx_hash::HashMap::default());
    let complete = std::cell::Cell::new(true);
    let mut resolved_spelling = |reference: &crate::ast::TypeRef| {
        let selected = structural_ids
            .next()
            .and_then(|syntax| semantics.selected_types.spelling(syntax))
            .unwrap_or_else(|| {
                complete.set(false);
                super::super::type_spellings::ResolvedTypeSpelling::default()
            });
        if let Some(ty) = selected.resolved {
            resolved
                .borrow_mut()
                .insert(reference as *const crate::ast::TypeRef as usize, ty);
        } else {
            complete.set(false);
        }
        selected
    };
    let mut resolve_argument = |argument: &crate::ast::TypeRef| {
        resolved
            .borrow()
            .get(&(argument as *const crate::ast::TypeRef as usize))
            .copied()
            .unwrap_or_else(|| {
                complete.set(false);
                crate::types::Ty::Error
            })
    };
    let mut alias_expansion = |identity| semantics.table.libraries.type_alias_expansion(identity);
    let spelled = spelling_of_ref_with(
        &materialized,
        scope,
        &context.expansions,
        &mut alias_expansion,
        spellings,
        &mut resolve_argument,
        &mut resolved_spelling,
    );
    if !complete.get() || structural_ids.next().is_some() {
        return None;
    }
    match semantics
        .selected_types
        .local_alias_projection(headers.syntax.written_type_id(ty))
    {
        Some(projection) => project_local_alias_spelling(semantics, context, &projection, spelled),
        None => Some(spelled),
    }
}

fn bound_spellings(
    semantics: &super::ProductionSignatureSemantics<'_>,
    scope_owner: DeclarationId,
    range: HeaderTypeBoundRange,
    parameters: &[String],
    scope: &TParams,
    context: &SpellingContext,
) -> Option<Vec<Vec<Spelled>>> {
    let headers = semantics.headers;
    parameters
        .iter()
        .map(|parameter| {
            headers
                .syntax
                .bounds(range)
                .iter()
                .filter(|bound| headers.lookup_names.get(bound.parameter) == Some(parameter))
                .map(|bound| spelling(headers, semantics, scope_owner, bound.ty, scope, context))
                .collect()
        })
        .collect()
}

fn partial_spelling(
    headers: &StreamedHeaderModule,
    semantics: &super::ProductionSignatureSemantics<'_>,
    scope_owner: DeclarationId,
    ty: HeaderTypeId,
    scope: &TParams,
    context: &SpellingContext,
    deferred: &crate::fx_hash::HashSet<HeaderTypeId>,
) -> Option<crate::spelling::DeferredSpellingRole> {
    if deferred.contains(&headers.syntax.written_type_id(ty)) {
        let written = headers
            .syntax
            .transient_written_type_ref(ty, &headers.lookup_names)?;
        let annotations = context
            .annotations
            .get(&headers.declarations.anchor(scope_owner)?.source)?;
        fn source_facts(
            reference: &crate::ast::TypeRef,
            annotations: &RecordedTypeAnnotations,
        ) -> Spelled {
            let args = if !reference.fun_params.is_empty() || reference.name == "<fun>" {
                let mut args = reference
                    .fun_params
                    .iter()
                    .map(|argument| source_facts(argument, annotations))
                    .collect::<Vec<_>>();
                if !reference.fun_suspend() {
                    args.push(
                        reference
                            .arg
                            .as_deref()
                            .map(|result| source_facts(result, annotations))
                            .unwrap_or_default(),
                    );
                }
                args
            } else {
                reference
                    .targs
                    .iter()
                    .map(|argument| source_facts(argument, annotations))
                    .collect()
            };
            Spelled {
                args,
                annotations: annotations.at(reference.span.lo).to_vec(),
                parameter_name: annotations
                    .parameter_name_at(reference.span.lo)
                    .map(Into::into),
                ..Spelled::default()
            }
        }
        return Some(crate::spelling::DeferredSpellingRole::Active(source_facts(
            &written,
            annotations,
        )));
    }
    let semantic = semantics
        .selected_types
        .spelling(headers.syntax.written_type_id(ty))?
        .resolved?;
    spelling(headers, semantics, scope_owner, ty, scope, context).map(|spelling| {
        crate::spelling::DeferredSpellingRole::Selected {
            semantic: Some(semantic),
            spelling,
        }
    })
}

fn selected_empty_spelling() -> crate::spelling::DeferredSpellingRole {
    crate::spelling::DeferredSpellingRole::Selected {
        semantic: None,
        spelling: Spelled::default(),
    }
}

/// Populate only stable declaration-keyed metadata sidecars from the compact Pass-1 inventory.
pub(in crate::resolve) fn finalized_compact_declared_spellings(
    semantics: &super::ProductionSignatureSemantics<'_>,
    index: &crate::fir::ResolvedModuleIndex,
    deferred_headers: &crate::fx_hash::HashSet<DeclarationId>,
    deferred_roles: &crate::fx_hash::HashMap<DeclarationId, crate::fx_hash::HashSet<HeaderTypeId>>,
) -> (
    Vec<(DeclarationId, DeclaredSpellings)>,
    Vec<(DeclarationId, crate::spelling::DeferredDeclaredSpellings)>,
    Vec<(DeclarationId, Spelled)>,
    Vec<DeclarationId>,
) {
    let headers = semantics.headers;
    let table = semantics.table;
    let annotations = (0..headers.sources.len() as u32)
        .map(crate::fir::SourceFileId::from_raw)
        .map(|source| {
            let annotations = crate::resolve::signature_collection::compact_source_type_annotations(
                table, headers, source,
            );
            (source, annotations)
        })
        .collect();
    let context = SpellingContext {
        expansions: table.alias_expansion_spellings.clone(),
        annotations,
    };
    let mut records = Vec::new();
    let mut deferred_records = Vec::new();
    let mut type_aliases = Vec::new();
    let mut failures = Vec::new();
    for stub in &headers.stubs {
        let Some(declaration) = headers.syntax.declaration(stub.id) else {
            continue;
        };
        if deferred_headers.contains(&stub.id)
            && matches!(
                declaration.kind,
                HeaderDeclarationKind::Classifier { .. } | HeaderDeclarationKind::TypeAlias { .. }
            )
        {
            // Active Pass 2 owns both the semantic classifier/alias header and its spelling.
            // DeferredDeclaredSpellings models only callable, property, and constructor roles.
            continue;
        }
        if let Some(deferred) = deferred_roles.get(&stub.id) {
            let record = (|| -> Option<crate::spelling::DeferredDeclaredSpellings> {
                match declaration.kind {
                    HeaderDeclarationKind::Callable {
                        receiver,
                        parameters,
                        result,
                        type_parameters,
                        bounds,
                        ..
                    } => {
                        let (own, scope) = declaration_scope(headers, stub.id, type_parameters)
                            .expect("compact callable scope must be materializable");
                        Some(crate::spelling::DeferredDeclaredSpellings {
                            ret: match result {
                                crate::fir::HeaderResultType::Explicit(result) => partial_spelling(
                                    headers, semantics, stub.id, result, &scope, &context, deferred,
                                )?,
                                crate::fir::HeaderResultType::ImplicitUnit
                                | crate::fir::HeaderResultType::Inferred => {
                                    selected_empty_spelling()
                                }
                            },
                            getter_ret: selected_empty_spelling(),
                            params: headers
                                .syntax
                                .parameters(parameters)
                                .iter()
                                .map(|parameter| {
                                    partial_spelling(
                                        headers,
                                        semantics,
                                        stub.id,
                                        parameter.ty,
                                        &scope,
                                        &context,
                                        deferred,
                                    )
                                })
                                .collect::<Option<Vec<_>>>()?,
                            receiver: match receiver {
                                Some(receiver) => partial_spelling(
                                    headers, semantics, stub.id, receiver, &scope, &context,
                                    deferred,
                                )?,
                                None => selected_empty_spelling(),
                            },
                            type_param_bounds: bound_spellings(
                                semantics, stub.id, bounds, &own, &scope, &context,
                            )?,
                        })
                    }
                    HeaderDeclarationKind::Property {
                        receiver,
                        declared_type,
                        getter_type,
                        type_parameters,
                        bounds,
                        context_parameters,
                        ..
                    } => {
                        let (own, scope) = declaration_scope(headers, stub.id, type_parameters)
                            .expect("compact property scope must be materializable");
                        Some(crate::spelling::DeferredDeclaredSpellings {
                            ret: match declared_type.or(getter_type) {
                                Some(ty) => partial_spelling(
                                    headers, semantics, stub.id, ty, &scope, &context, deferred,
                                )?,
                                None => selected_empty_spelling(),
                            },
                            getter_ret: match getter_type {
                                Some(ty) => partial_spelling(
                                    headers, semantics, stub.id, ty, &scope, &context, deferred,
                                )?,
                                None => selected_empty_spelling(),
                            },
                            params: headers
                                .syntax
                                .parameters(context_parameters)
                                .iter()
                                .map(|parameter| {
                                    partial_spelling(
                                        headers,
                                        semantics,
                                        stub.id,
                                        parameter.ty,
                                        &scope,
                                        &context,
                                        deferred,
                                    )
                                })
                                .collect::<Option<Vec<_>>>()?,
                            receiver: match receiver {
                                Some(receiver) => partial_spelling(
                                    headers, semantics, stub.id, receiver, &scope, &context,
                                    deferred,
                                )?,
                                None => selected_empty_spelling(),
                            },
                            type_param_bounds: bound_spellings(
                                semantics, stub.id, bounds, &own, &scope, &context,
                            )?,
                        })
                    }
                    HeaderDeclarationKind::Constructor { parameters, .. } => {
                        let (_, scope) = declaration_scope(
                            headers,
                            stub.id,
                            HeaderTypeParameterRange::default(),
                        )
                        .expect("compact constructor scope must be materializable");
                        Some(crate::spelling::DeferredDeclaredSpellings {
                            ret: selected_empty_spelling(),
                            getter_ret: selected_empty_spelling(),
                            params: headers
                                .syntax
                                .parameters(parameters)
                                .iter()
                                .map(|parameter| {
                                    partial_spelling(
                                        headers,
                                        semantics,
                                        stub.id,
                                        parameter.ty,
                                        &scope,
                                        &context,
                                        deferred,
                                    )
                                })
                                .collect::<Option<Vec<_>>>()?,
                            receiver: selected_empty_spelling(),
                            type_param_bounds: Vec::new(),
                        })
                    }
                    HeaderDeclarationKind::Classifier { .. }
                    | HeaderDeclarationKind::TypeAlias { .. } => None,
                }
            })();
            match record {
                Some(record) => deferred_records.push((stub.id, record)),
                None => failures.push(stub.id),
            }
            continue;
        }
        let record = (|| -> Option<DeclaredSpellings> {
            match declaration.kind {
                HeaderDeclarationKind::Callable {
                    receiver,
                    parameters,
                    result,
                    type_parameters,
                    bounds,
                    ..
                } => {
                    let (own, scope) = declaration_scope(headers, stub.id, type_parameters)
                        .expect("compact callable scope must be materializable");
                    let ret = match result {
                        crate::fir::HeaderResultType::Explicit(result) => {
                            spelling(headers, semantics, stub.id, result, &scope, &context)
                        }
                        crate::fir::HeaderResultType::ImplicitUnit
                        | crate::fir::HeaderResultType::Inferred => Some(Spelled::default()),
                    };
                    Some(DeclaredSpellings {
                        ret: ret?,
                        params: headers
                            .syntax
                            .parameters(parameters)
                            .iter()
                            .map(|parameter| {
                                spelling(
                                    headers,
                                    semantics,
                                    stub.id,
                                    parameter.ty,
                                    &scope,
                                    &context,
                                )
                            })
                            .collect::<Option<Vec<_>>>()?,
                        receiver: match receiver {
                            Some(receiver) => {
                                spelling(headers, semantics, stub.id, receiver, &scope, &context)?
                            }
                            None => Spelled::default(),
                        },
                        type_param_bounds: bound_spellings(
                            semantics, stub.id, bounds, &own, &scope, &context,
                        )?,
                        ..DeclaredSpellings::default()
                    })
                }
                HeaderDeclarationKind::Property {
                    receiver,
                    declared_type,
                    getter_type,
                    type_parameters,
                    bounds,
                    context_parameters,
                    ..
                } => {
                    let (own, scope) = declaration_scope(headers, stub.id, type_parameters)
                        .expect("compact property scope must be materializable");
                    Some(DeclaredSpellings {
                        // An explicit getter return is the property's declared public type when
                        // the property header itself omits one. Semantic header projection uses
                        // this same stable pair; metadata spelling must join it to the property
                        // declaration here instead of treating the body as inferred. Keep the
                        // getter role as a distinct occurrence for getter-specific checks.
                        ret: match declared_type.or(getter_type) {
                            Some(ty) => {
                                spelling(headers, semantics, stub.id, ty, &scope, &context)?
                            }
                            None => Spelled::default(),
                        },
                        getter_ret: match getter_type {
                            Some(ty) => {
                                spelling(headers, semantics, stub.id, ty, &scope, &context)?
                            }
                            None => Spelled::default(),
                        },
                        receiver: match receiver {
                            Some(ty) => {
                                spelling(headers, semantics, stub.id, ty, &scope, &context)?
                            }
                            None => Spelled::default(),
                        },
                        params: headers
                            .syntax
                            .parameters(context_parameters)
                            .iter()
                            .map(|parameter| {
                                spelling(
                                    headers,
                                    semantics,
                                    stub.id,
                                    parameter.ty,
                                    &scope,
                                    &context,
                                )
                            })
                            .collect::<Option<Vec<_>>>()?,
                        type_param_bounds: bound_spellings(
                            semantics, stub.id, bounds, &own, &scope, &context,
                        )?,
                        ..DeclaredSpellings::default()
                    })
                }
                HeaderDeclarationKind::Classifier {
                    type_parameters,
                    bounds,
                    supertypes,
                    base,
                    context_parameters,
                    primary_parameters,
                    ..
                } => {
                    let (own, scope) = declaration_scope(headers, stub.id, type_parameters)
                        .expect("compact classifier scope must be materializable");
                    // Declaration ancestry is still owned by the compact header inventory here;
                    // `ResolvedModuleIndex` receives that inventory only after signature
                    // finalization succeeds. Querying the index would therefore miss every primary
                    // constructor and try to spell parameters whose header is deliberately deferred.
                    let primary_constructor = headers
                        .owned_stubs(stub.id)
                        .find(|candidate| {
                            candidate.kind == crate::fir::DeclarationKind::Constructor
                                && headers
                                    .declarations
                                    .anchor(candidate.id)
                                    .is_some_and(|anchor| anchor.sibling == 0)
                        })
                        .map(|constructor| constructor.id);
                    let primary_parameters_deferred =
                        primary_constructor.is_some_and(|constructor| {
                            deferred_headers.contains(&constructor)
                                || deferred_roles.contains_key(&constructor)
                        });
                    Some(DeclaredSpellings {
                        params: if primary_parameters_deferred {
                            Vec::new()
                        } else {
                            headers
                                .syntax
                                .parameters(primary_parameters)
                                .iter()
                                .map(|parameter| {
                                    spelling(
                                        headers,
                                        semantics,
                                        stub.id,
                                        parameter.ty,
                                        &scope,
                                        &context,
                                    )
                                })
                                .collect::<Option<Vec<_>>>()?
                        },
                        classifier_context_params: headers
                            .syntax
                            .parameters(context_parameters)
                            .iter()
                            .map(|parameter| {
                                spelling(
                                    headers,
                                    semantics,
                                    stub.id,
                                    parameter.ty,
                                    &scope,
                                    &context,
                                )
                            })
                            .collect::<Option<Vec<_>>>()?,
                        superclass: match base {
                            Some(ty) => {
                                spelling(headers, semantics, stub.id, ty, &scope, &context)?
                            }
                            None => Spelled::default(),
                        },
                        supertypes: headers
                            .syntax
                            .type_operands(supertypes)
                            .iter()
                            .map(|ty| spelling(headers, semantics, stub.id, *ty, &scope, &context))
                            .collect::<Option<Vec<_>>>()?,
                        type_param_bounds: bound_spellings(
                            semantics, stub.id, bounds, &own, &scope, &context,
                        )?,
                        ..DeclaredSpellings::default()
                    })
                }
                HeaderDeclarationKind::Constructor {
                    parameters,
                    context_parameters: _,
                } => {
                    let (_, scope) =
                        declaration_scope(headers, stub.id, HeaderTypeParameterRange::default())
                            .expect("compact constructor scope must be materializable");
                    Some(DeclaredSpellings {
                        params: headers
                            .syntax
                            .parameters(parameters)
                            .iter()
                            .map(|parameter| {
                                spelling(
                                    headers,
                                    semantics,
                                    stub.id,
                                    parameter.ty,
                                    &scope,
                                    &context,
                                )
                            })
                            .collect::<Option<Vec<_>>>()?,
                        ..DeclaredSpellings::default()
                    })
                }
                HeaderDeclarationKind::TypeAlias {
                    type_parameters,
                    target,
                } => {
                    if index.type_alias_header(stub.id).is_none() {
                        return Some(DeclaredSpellings::default());
                    }
                    let (_, scope) = declaration_scope(headers, stub.id, type_parameters)
                        .expect("compact type-alias scope must be materializable");
                    type_aliases.push((
                        stub.id,
                        spelling(headers, semantics, stub.id, target, &scope, &context)?,
                    ));
                    Some(DeclaredSpellings::default())
                }
            }
        })();
        match record {
            // A header still owned by Pass 2 delegates its source projection to the same active
            // lexical checker. Successful empty records for other local declarations remain
            // explicit because downstream spelling consumers must distinguish them from absence.
            Some(record)
                if !record.is_none()
                    || stub.flags.has(crate::fir::DeclarationFlags::LOCAL_CLASS) =>
            {
                records.push((stub.id, record));
            }
            Some(_) => {}
            None => failures.push(stub.id),
        }
    }
    (records, deferred_records, type_aliases, failures)
}
