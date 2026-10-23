//! Pass-2 publication of source spellings owned by deferred local declaration headers.

use super::*;
use crate::fir::{DeclarationId, DeclarationKind, ResolvedModuleIndex};
use crate::resolve::TypeInfo;
use crate::spelling::{DeclaredSpellings, Spelled};

fn spelling(info: &TypeInfo, reference: &TypeRef) -> Option<Spelled> {
    info.resolved_declaration_spelling(reference).cloned()
}

fn bound_spellings(
    info: &TypeInfo,
    parameters: &[String],
    bounds: &[(String, TypeRef)],
) -> Option<Vec<Vec<Spelled>>> {
    parameters
        .iter()
        .map(|parameter| {
            bounds
                .iter()
                .filter(|(owner, _)| owner == parameter)
                .map(|(_, bound)| spelling(info, bound))
                .collect()
        })
        .collect()
}

fn classifier_spellings(info: &TypeInfo, class: &ClassDecl) -> Option<DeclaredSpellings> {
    let superclass = match class.base_class.as_deref() {
        Some(base) => {
            let reference = super::super::base_class_type_ref(
                base,
                &class.base_type_args,
                class.base_class_span.unwrap_or(class.span),
            );
            spelling(info, &reference)?
        }
        None => Spelled::default(),
    };
    Some(DeclaredSpellings {
        params: class
            .props
            .iter()
            .map(|parameter| spelling(info, &parameter.ty))
            .collect::<Option<Vec<_>>>()?,
        classifier_context_params: class
            .context_params
            .iter()
            .map(|parameter| spelling(info, &parameter.ty))
            .collect::<Option<Vec<_>>>()?,
        superclass,
        supertypes: class
            .supertypes
            .iter()
            .map(|supertype| spelling(info, supertype))
            .collect::<Option<Vec<_>>>()?,
        type_param_bounds: bound_spellings(info, &class.type_params, &class.type_param_bounds)?,
        ..DeclaredSpellings::default()
    })
}

fn function_spellings(info: &TypeInfo, function: &FunDecl) -> Option<DeclaredSpellings> {
    let ret = match function.ret.as_ref() {
        Some(result) => spelling(info, result)?,
        None => Spelled::default(),
    };
    let receiver = match function.receiver.as_ref() {
        Some(receiver) => spelling(info, receiver)?,
        None => Spelled::default(),
    };
    Some(DeclaredSpellings {
        ret,
        params: function
            .params
            .iter()
            .map(|parameter| spelling(info, &parameter.ty))
            .collect::<Option<Vec<_>>>()?,
        receiver,
        type_param_bounds: bound_spellings(
            info,
            &function.type_params,
            &function.type_param_bounds,
        )?,
        ..DeclaredSpellings::default()
    })
}

fn property_spellings(info: &TypeInfo, property: &PropDecl) -> Option<DeclaredSpellings> {
    let ret = match property.ty.as_ref().or(property.getter_ty.as_ref()) {
        Some(declared) => spelling(info, declared)?,
        None => Spelled::default(),
    };
    let getter_ret = match property.getter_ty.as_ref() {
        Some(declared) => spelling(info, declared)?,
        None => Spelled::default(),
    };
    let receiver = match property.receiver.as_ref() {
        Some(receiver) => spelling(info, receiver)?,
        None => Spelled::default(),
    };
    Some(DeclaredSpellings {
        ret,
        getter_ret,
        params: property
            .context_params
            .iter()
            .map(|parameter| spelling(info, &parameter.ty))
            .collect::<Option<Vec<_>>>()?,
        receiver,
        type_param_bounds: bound_spellings(
            info,
            &property.type_params,
            &property.type_param_bounds,
        )?,
        ..DeclaredSpellings::default()
    })
}

/// Transfer already-bound active syntax into the stable declaration roles published alongside the
/// local signature. This is a structural join through `ActiveSourceDeclarations`; it performs no
/// name lookup and never resolves a type reference.
pub(super) fn publish_checked_local_declaration_spellings(
    file: &File,
    active: &crate::fir::ActiveSourceDeclarations,
    info: &TypeInfo,
    index: &mut ResolvedModuleIndex,
    declarations: &[DeclarationId],
    pass_one_finalized_headers: &crate::fx_hash::HashSet<DeclarationId>,
    pass_one_deferred_spellings: &crate::fx_hash::HashSet<DeclarationId>,
) -> Vec<DeclarationId> {
    let mut failed = Vec::new();
    for &declaration in declarations {
        let Some(anchor) = index.declaration_anchor(declaration) else {
            failed.push(declaration);
            continue;
        };
        if let Some(existing) = index.declaration_spellings(declaration) {
            let Some((_, class)) = (anchor.kind == DeclarationKind::Classifier)
                .then(|| active.class(file, declaration))
                .flatten()
            else {
                continue;
            };
            if existing.params.len() == class.props.len() {
                continue;
            }
            let primary_constructor =
                index.owned_declaration(declaration, DeclarationKind::Constructor, 0);
            let deferred_primary = !class.props.is_empty()
                && existing.params.is_empty()
                && primary_constructor.is_some_and(|constructor| {
                    !pass_one_finalized_headers.contains(&constructor)
                        || pass_one_deferred_spellings.contains(&constructor)
                });
            if !deferred_primary {
                failed.push(declaration);
                continue;
            }
            let parameters = active.class(file, declaration).and_then(|(_, class)| {
                class
                    .props
                    .iter()
                    .map(|parameter| spelling(info, &parameter.ty))
                    .collect::<Option<Vec<_>>>()
            });
            match parameters {
                Some(parameters) => {
                    index.complete_classifier_primary_parameter_spellings(declaration, parameters)
                }
                None => failed.push(declaration),
            }
            continue;
        }
        if index.declaration_header(declaration).is_some_and(|header| {
            header
                .flags
                .has(crate::fir::DeclarationFlags::COMPILER_GENERATED)
        }) {
            // Generated data/value methods and capture-storage properties have no parser
            // declaration and therefore no written type occurrences to transfer. Their Pass-1
            // semantic publication is complete; publish the explicit empty spelling record that
            // downstream source-rendering consumers expect. Capture storage deliberately has no
            // source property identity.
            let complete = match anchor.kind {
                DeclarationKind::Function => {
                    index.signature(declaration).is_some()
                        && index.callable_for_declaration(declaration).is_some()
                }
                DeclarationKind::Property => index.signature(declaration).is_some(),
                DeclarationKind::Classifier
                | DeclarationKind::EnumEntry
                | DeclarationKind::TypeAlias
                | DeclarationKind::Constructor
                | DeclarationKind::Accessor
                | DeclarationKind::Initializer => false,
            };
            if complete {
                index.publish_declaration_spellings(declaration, DeclaredSpellings::default());
            } else {
                failed.push(declaration);
            }
            continue;
        }
        if pass_one_finalized_headers.contains(&declaration)
            && !pass_one_deferred_spellings.contains(&declaration)
        {
            // Absence is not an ownership marker. A complete Pass-1 semantic header may publish
            // from active TypeInfo only when Pass 1 explicitly transferred its written
            // occurrences; every other missing sidecar is a broken Pass-1 publication.
            failed.push(declaration);
            continue;
        }
        let record = match anchor.kind {
            DeclarationKind::Classifier => active
                .class(file, declaration)
                .and_then(|(_, class)| classifier_spellings(info, class)),
            DeclarationKind::Function => active
                .function(file, declaration)
                .and_then(|function| function_spellings(info, function)),
            DeclarationKind::Property => {
                if let Some(parameter) = active.constructor_parameter(file, declaration) {
                    spelling(info, &parameter.ty).map(|ret| DeclaredSpellings {
                        ret,
                        ..DeclaredSpellings::default()
                    })
                } else {
                    active
                        .property(file, declaration)
                        .and_then(|property| property_spellings(info, property))
                }
            }
            DeclarationKind::Constructor => {
                let Some(owner) = anchor.owner else {
                    failed.push(declaration);
                    continue;
                };
                active.class(file, owner).and_then(|(_, class)| {
                    let parameters: Vec<&TypeRef> = if anchor.sibling == 0 {
                        class.props.iter().map(|parameter| &parameter.ty).collect()
                    } else {
                        class
                            .secondary_ctors
                            .get(anchor.sibling.saturating_sub(1) as usize)?
                            .params
                            .iter()
                            .map(|parameter| &parameter.ty)
                            .collect()
                    };
                    parameters
                        .into_iter()
                        .map(|parameter| spelling(info, parameter))
                        .collect::<Option<Vec<_>>>()
                        .map(|params| DeclaredSpellings {
                            params,
                            ..DeclaredSpellings::default()
                        })
                })
            }
            DeclarationKind::Accessor => Some(DeclaredSpellings::default()),
            DeclarationKind::EnumEntry
            | DeclarationKind::TypeAlias
            | DeclarationKind::Initializer => continue,
        };
        match record {
            Some(record) => index.publish_declaration_spellings(declaration, record),
            None => failed.push(declaration),
        }
    }
    failed
}
