//! Stable metadata spelling publication from compact declaration headers.
//!
//! Source type aliases may be expanded by the parser before semantic resolution. The compact
//! header arena owns both the expanded type and its as-written counterpart by `HeaderTypeId`, so
//! production signature collection never reopens `File::alias_spellings` or walks declarations by
//! parser coordinates.

use super::super::{spelling_of_ref_with, spelling_scope, ClassNames, SymbolTable, TParams};
use crate::fir::{
    DeclarationId, DeclarationKind, HeaderDeclarationKind, HeaderTypeBoundRange, HeaderTypeId,
    HeaderTypeParameterRange, StreamedHeaderModule,
};
use crate::spelling::{DeclaredSpellings, Spelled};

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
    classes: &ClassNames,
    scope: &TParams,
    expansions: &std::collections::HashMap<
        crate::types::TypeName,
        (Spelled, Vec<String>, crate::types::Ty),
    >,
) -> Option<Spelled> {
    let materialized = headers
        .syntax
        .transient_type_ref(ty, &headers.lookup_names)?;
    let source_spellings = headers
        .syntax
        .transient_source_spellings(ty, &headers.lookup_names)?;
    let signature_scope = crate::fir::SignatureScope {
        owner: scope_owner,
        source: headers.declarations.anchor(scope_owner)?.source,
    };
    let mut resolve_argument = |argument: &crate::ast::TypeRef| {
        semantics
            .resolve_signature_type_reference(signature_scope, argument)
            .unwrap_or(crate::types::Ty::Error)
    };
    Some(spelling_of_ref_with(
        &materialized,
        classes,
        scope,
        expansions,
        &source_spellings,
        &mut resolve_argument,
    ))
}

fn bound_spellings(
    semantics: &super::ProductionSignatureSemantics<'_>,
    scope_owner: DeclarationId,
    range: HeaderTypeBoundRange,
    parameters: &[String],
    classes: &ClassNames,
    scope: &TParams,
    expansions: &std::collections::HashMap<
        crate::types::TypeName,
        (Spelled, Vec<String>, crate::types::Ty),
    >,
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
                .map(|bound| {
                    spelling(
                        headers,
                        semantics,
                        scope_owner,
                        bound.ty,
                        classes,
                        scope,
                        expansions,
                    )
                })
                .collect()
        })
        .collect()
}

/// Populate only stable declaration-keyed metadata sidecars from the compact Pass-1 inventory.
pub(in crate::resolve) fn collect_compact_declared_spellings(
    table: &mut SymbolTable,
    headers: &StreamedHeaderModule,
    file_class_names: &[ClassNames],
) {
    let expansions = table.alias_expansion_spellings.clone();
    let classifier_types = table
        .classes
        .values()
        .filter_map(|classifier| {
            classifier
                .stable_declaration
                .map(|declaration| (declaration, classifier.internal))
        })
        .collect();
    let empty_receivers = std::collections::HashMap::new();
    let semantics = super::ProductionSignatureSemantics {
        headers,
        table,
        classifier_types: &classifier_types,
        parameters: std::collections::HashMap::new(),
        extension_receivers: &empty_receivers,
        source_orders: std::collections::HashMap::new(),
        signature_origins: std::collections::HashMap::new(),
        scoped_receivers: std::cell::RefCell::new(std::collections::HashMap::new()),
        scoped_constraint_inputs: std::cell::RefCell::new(std::collections::HashMap::new()),
        scoped_constraints: std::cell::RefCell::new(std::collections::HashMap::new()),
        completed_scoped_constraints: std::cell::RefCell::new(std::collections::HashMap::new()),
        diagnostics: std::cell::RefCell::new(Vec::new()),
    };
    let mut records = Vec::new();
    for stub in &headers.stubs {
        let Some(declaration) = headers.syntax.declaration(stub.id) else {
            continue;
        };
        let Some(classes) = file_class_names.get(stub.source.raw() as usize) else {
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
                        crate::fir::HeaderResultType::Explicit(result) => spelling(
                            headers,
                            &semantics,
                            stub.id,
                            result,
                            classes,
                            &scope,
                            &expansions,
                        ),
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
                                    &semantics,
                                    stub.id,
                                    parameter.ty,
                                    classes,
                                    &scope,
                                    &expansions,
                                )
                            })
                            .collect::<Option<Vec<_>>>()?,
                        receiver: match receiver {
                            Some(receiver) => spelling(
                                headers,
                                &semantics,
                                stub.id,
                                receiver,
                                classes,
                                &scope,
                                &expansions,
                            )?,
                            None => Spelled::default(),
                        },
                        type_param_bounds: bound_spellings(
                            &semantics,
                            stub.id,
                            bounds,
                            &own,
                            classes,
                            &scope,
                            &expansions,
                        )?,
                        ..DeclaredSpellings::default()
                    })
                }
                HeaderDeclarationKind::Property {
                    receiver,
                    declared_type,
                    type_parameters,
                    bounds,
                    ..
                } => {
                    let (own, scope) = declaration_scope(headers, stub.id, type_parameters)
                        .expect("compact property scope must be materializable");
                    Some(DeclaredSpellings {
                        ret: match declared_type {
                            Some(ty) => spelling(
                                headers,
                                &semantics,
                                stub.id,
                                ty,
                                classes,
                                &scope,
                                &expansions,
                            )?,
                            None => Spelled::default(),
                        },
                        receiver: match receiver {
                            Some(ty) => spelling(
                                headers,
                                &semantics,
                                stub.id,
                                ty,
                                classes,
                                &scope,
                                &expansions,
                            )?,
                            None => Spelled::default(),
                        },
                        type_param_bounds: bound_spellings(
                            &semantics,
                            stub.id,
                            bounds,
                            &own,
                            classes,
                            &scope,
                            &expansions,
                        )?,
                        ..DeclaredSpellings::default()
                    })
                }
                HeaderDeclarationKind::Classifier {
                    type_parameters,
                    bounds,
                    supertypes,
                    base,
                    primary_parameters,
                    ..
                } => {
                    let (own, scope) = declaration_scope(headers, stub.id, type_parameters)
                        .expect("compact classifier scope must be materializable");
                    Some(DeclaredSpellings {
                        params: headers
                            .syntax
                            .parameters(primary_parameters)
                            .iter()
                            .map(|parameter| {
                                spelling(
                                    headers,
                                    &semantics,
                                    stub.id,
                                    parameter.ty,
                                    classes,
                                    &scope,
                                    &expansions,
                                )
                            })
                            .collect::<Option<Vec<_>>>()?,
                        superclass: match base {
                            Some(ty) => spelling(
                                headers,
                                &semantics,
                                stub.id,
                                ty,
                                classes,
                                &scope,
                                &expansions,
                            )?,
                            None => Spelled::default(),
                        },
                        supertypes: headers
                            .syntax
                            .type_operands(supertypes)
                            .iter()
                            .map(|ty| {
                                spelling(
                                    headers,
                                    &semantics,
                                    stub.id,
                                    *ty,
                                    classes,
                                    &scope,
                                    &expansions,
                                )
                            })
                            .collect::<Option<Vec<_>>>()?,
                        type_param_bounds: bound_spellings(
                            &semantics,
                            stub.id,
                            bounds,
                            &own,
                            classes,
                            &scope,
                            &expansions,
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
                                    &semantics,
                                    stub.id,
                                    parameter.ty,
                                    classes,
                                    &scope,
                                    &expansions,
                                )
                            })
                            .collect::<Option<Vec<_>>>()?,
                        ..DeclaredSpellings::default()
                    })
                }
                HeaderDeclarationKind::TypeAlias { .. } => None,
            }
        })();
        if let Some(record) = record.filter(|record| !record.is_none()) {
            records.push((stub.id, record));
        }
    }
    drop(semantics);
    table.stable_declared_spellings.extend(records);
}
