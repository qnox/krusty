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
    (complete.get() && structural_ids.next().is_none()).then_some(spelled)
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

/// Populate only stable declaration-keyed metadata sidecars from the compact Pass-1 inventory.
pub(in crate::resolve) fn finalized_compact_declared_spellings(
    semantics: &super::ProductionSignatureSemantics<'_>,
    index: &crate::fir::ResolvedModuleIndex,
    deferred_headers: &crate::fx_hash::HashSet<DeclarationId>,
) -> (
    Vec<(DeclarationId, DeclaredSpellings)>,
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
    let mut type_aliases = Vec::new();
    let mut failures = Vec::new();
    for stub in &headers.stubs {
        let Some(declaration) = headers.syntax.declaration(stub.id) else {
            continue;
        };
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
                    let primary_parameters_deferred = primary_constructor
                        .is_some_and(|constructor| deferred_headers.contains(&constructor));
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
            _ if deferred_headers.contains(&stub.id) => {}
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
    (records, type_aliases, failures)
}
