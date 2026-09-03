//! Temporary projection from the finalized stable signature index into the legacy checker model.
//!
//! Pass 2 is being migrated to consume [`ResolvedModuleIndex`] directly. Until the legacy
//! `SymbolTable` disappears from that pass, every declaration-backed type it exposes must agree
//! with the compact solver. This adapter is deliberately keyed only by stable declaration identity;
//! it performs no source lookup and no spelling-based matching.

use super::{Signature, SymbolTable};
use crate::fir::{DeclarationId, ResolvedModuleIndex};

fn resolved_result(
    index: &ResolvedModuleIndex,
    declaration: Option<DeclarationId>,
) -> Option<crate::types::Ty> {
    index
        .signature(declaration?)
        .map(|signature| signature.result.get())
}

fn project_callable(index: &ResolvedModuleIndex, signature: &mut Signature) {
    let Some(declaration) = signature.stable_declaration else {
        return;
    };
    let Some(result) = resolved_result(index, Some(declaration)) else {
        return;
    };
    signature.set_inferred_return(result);
    signature.contract = index
        .contract(declaration)
        .map(|contract| contract.to_arc());
}

fn project_class_callable(
    index: &ResolvedModuleIndex,
    signature: &mut Signature,
    anonymous: bool,
    class_formals: &[String],
) {
    project_callable(index, signature);
    if anonymous {
        signature.set_inferred_return(classifier_relative_type(signature.ret, class_formals));
    }
}

/// Convert a contextually solved anonymous-member type back to the anonymous classifier's own
/// declaration variables. The parser copies visible lexical parameter spellings into an anonymous
/// classifier's compact header because the classifier is hoisted out of its expression arena. The
/// solver correctly sees the enclosing declaration identities while checking the expression, but
/// the provider template stored on the synthetic classifier must use its own copied formals so an
/// applied receiver can specialize it normally.
fn classifier_relative_type(ty: crate::types::Ty, formals: &[String]) -> crate::types::Ty {
    let mut occurrences = Vec::new();
    super::ty_param_names_into(ty, &mut occurrences);
    let mut identities = std::collections::HashMap::new();
    for occurrence in occurrences {
        let source = crate::types::type_parameter_source_name(occurrence);
        let Some(formal) = formals
            .iter()
            .find(|formal| crate::types::type_parameter_source_name(formal) == source)
        else {
            continue;
        };
        if occurrence != formal {
            identities.insert(occurrence, crate::types::intern(formal));
        }
    }
    crate::types::ty_rename_params(ty, &identities)
}

/// Project the compact solver's already-finalized direct parents into the temporary legacy class
/// view. Applied-hierarchy publication still walks `SymbolTable` during the migration, so leaving
/// provisional parser/checker parent shapes here would let `Ty::Error` escape into stable FIR
/// metadata. This is a structural copy of resolved facts, not another supertype-resolution path.
fn project_classifier_parents(
    index: &ResolvedModuleIndex,
    class: &mut super::ClassSig,
    anonymous: bool,
) {
    let Some(header) = class
        .stable_declaration
        .and_then(|declaration| index.classifier_header(declaration))
    else {
        return;
    };
    let relative = |ty: crate::types::Ty| {
        if anonymous {
            classifier_relative_type(ty, &class.type_parameters.type_params)
        } else {
            ty
        }
    };
    let object_parent_parts = |ty: crate::types::Ty| {
        let ty = relative(ty);
        let owner = ty
            .obj_internal()
            .expect("a finalized superclass must name a classifier");
        (owner, ty.type_args().to_vec())
    };

    let superclass = header
        .superclass
        .map(|parent| object_parent_parts(parent.get()));
    class.super_internal = superclass.as_ref().map(|(owner, _)| *owner);
    class.super_type_args = superclass
        .map(|(_, arguments)| arguments)
        .unwrap_or_default();

    let mut interfaces = Vec::new();
    let mut callable_signatures = Vec::new();
    for parent in &header.interfaces {
        let parent = relative(parent.get());
        if let Some(owner) = parent.obj_internal() {
            interfaces.push((owner, parent.type_args().to_vec()));
        } else if matches!(parent, crate::types::Ty::Fun(_)) {
            // A function supertype is a complete semantic callable shape, not an applied nominal
            // classifier. Keep it in the dedicated source-model channel; a target backend may later
            // choose its own nominal representation without that identity entering stable FIR.
            callable_signatures.push(parent);
        } else {
            panic!("a finalized direct interface must name a classifier or function type");
        }
    }
    class.callable_signature = callable_signatures.first().copied();
    class.callable_signatures = callable_signatures;
    let mut names = crate::types::TypeNameList::new();
    for (owner, _) in &interfaces {
        names.push_name(*owner);
    }
    class.interfaces = names;
    class.interface_type_args = interfaces
        .into_iter()
        .map(|(_, arguments)| arguments)
        .collect();
}

/// Make the still-active legacy checker view agree with the pending-free Pass-1 product.
///
/// This function may shrink as checker queries move to `ResolvedModuleIndex`; it must never grow
/// lookup or inference behavior. The compact solver has already made every semantic decision.
pub(crate) fn project_finalized_signatures(index: &ResolvedModuleIndex, table: &mut SymbolTable) {
    let anonymous_classifiers = table
        .anonymous_object_types
        .values()
        .copied()
        .collect::<std::collections::HashSet<_>>();
    let mut stable_spellings = Vec::new();
    for signature in table.funs.values().flatten().chain(
        table
            .ext_funs
            .values()
            .flat_map(std::collections::HashMap::values)
            .flatten(),
    ) {
        let (Some(stable), Some(source), Some(declaration)) = (
            signature.stable_declaration,
            signature.source_file,
            signature.source_decl,
        ) else {
            continue;
        };
        if let Some(spelling) = table.declared_spellings.get(&(source, declaration)) {
            stable_spellings.push((stable, spelling.clone()));
        }
    }
    for (&(source, declaration), property) in &table.source_props {
        let Some(stable) = property.stable_declaration else {
            continue;
        };
        if let Some(spelling) = table
            .declared_spellings
            .get(&(source, crate::ast::DeclId(declaration)))
        {
            stable_spellings.push((stable, spelling.clone()));
        }
    }
    for property in table.ext_props.values().flatten() {
        let Some(stable) = property.stable_declaration else {
            continue;
        };
        let (source, declaration) = property.source;
        if let Some(spelling) = table
            .declared_spellings
            .get(&(source, crate::ast::DeclId(declaration)))
        {
            stable_spellings.push((stable, spelling.clone()));
        }
    }
    for class in table.classes.values() {
        let (Some(stable), Some(declaration)) = (class.stable_declaration, class.source_decl)
        else {
            continue;
        };
        if let Some(spelling) = table
            .declared_spellings
            .get(&(class.source_file, declaration))
        {
            stable_spellings.push((stable, spelling.clone()));
        }
        for signature in class.methods.values().flatten().chain(
            class
                .member_ext_funs
                .values()
                .flatten()
                .map(|function| &function.signature),
        ) {
            let (Some(stable), Some(source_member)) =
                (signature.stable_declaration, signature.source_member)
            else {
                continue;
            };
            if let Some(spelling) = table.member_spellings.get(&source_member) {
                stable_spellings.push((stable, spelling.clone()));
            }
        }
        for property in class
            .declared_props
            .values()
            .chain(class.contextual_props.values().flatten())
        {
            let (Some(stable), Some(source_member)) =
                (property.stable_declaration, property.source_member)
            else {
                continue;
            };
            if let Some(spelling) = table.member_spellings.get(&source_member) {
                stable_spellings.push((stable, spelling.clone()));
            }
        }
        for property in class.member_ext_props.values().flatten() {
            let (Some(stable), Some(source_member)) =
                (property.stable_declaration, property.source_member)
            else {
                continue;
            };
            if let Some(spelling) = table.member_spellings.get(&source_member) {
                stable_spellings.push((stable, spelling.clone()));
            }
        }
    }
    table.stable_declared_spellings.extend(stable_spellings);

    // Signature collection may already have populated the derived ModuleSymbols cache with the
    // provisional `Pending` shapes needed by demand-driven solving. Projection changes those
    // declarations in place, so invalidate that cache as one atomic module mutation before any
    // Pass-2 checker can observe it.
    table.begin_module_mutation();
    for signature in table.funs.values_mut().flatten() {
        project_callable(index, signature);
    }
    for signature in table
        .ext_funs
        .values_mut()
        .flat_map(std::collections::HashMap::values_mut)
        .flatten()
    {
        project_callable(index, signature);
    }

    for property in table.source_props.values_mut() {
        let Some(signature) = property
            .stable_declaration
            .and_then(|declaration| index.signature(declaration))
        else {
            continue;
        };
        crate::trace_compiler!(
            "signature",
            "project top-level property name={} declaration={:?} prior={:?} resolved={:?}",
            property.name,
            property.stable_declaration,
            property.ty,
            signature.result.get(),
        );
        property.ty = signature.result.get();
        property.storage_ty = index
            .property_for_declaration(property.stable_declaration.expect("checked above"))
            .and_then(|property| index.property(property))
            .and_then(|property| property.storage_type)
            .map(|storage| storage.get())
            .or(property
                .storage_ty
                .filter(|storage| *storage != crate::types::Ty::Pending));
        property.context_params = signature
            .parameters
            .iter()
            .map(|parameter| parameter.get())
            .collect();
        // `source_props` is the stable declaration-backed provider view. The legacy Pass-2
        // checker still has one top-level value cache keyed by spelling; keep that migration view
        // synchronized after the compact solver has made the semantic decision. This is a pure
        // projection of the already-selected declaration result, not a second inference path.
        // Contextual properties do not inhabit this cache, matching signature collection.
        if property.context_params.is_empty() {
            if let Some((legacy, _, _)) = table.props.get_mut(&property.name) {
                *legacy = signature.result.get();
            }
        }
    }
    for property in table.ext_props.values_mut().flatten() {
        let Some(declaration) = property.stable_declaration else {
            continue;
        };
        let Some(signature) = index.signature(declaration) else {
            continue;
        };
        property.ty = signature.result.get();
        property.context_params = signature
            .parameters
            .iter()
            .map(|parameter| parameter.get())
            .collect();
        if let Some(receiver) = index
            .property_for_declaration(declaration)
            .and_then(|property| index.property(property))
            .and_then(|property| property.extension_receiver)
        {
            property.receiver = receiver.get();
        }
    }

    for class in table.classes.values_mut() {
        let anonymous = anonymous_classifiers.contains(&class.internal);
        project_classifier_parents(index, class, anonymous);
        for plan in &mut class.source_methods {
            project_class_callable(
                index,
                &mut plan.signature,
                anonymous,
                &class.type_parameters.type_params,
            );
        }
        for signature in class.methods.values_mut().flatten() {
            project_class_callable(
                index,
                signature,
                anonymous,
                &class.type_parameters.type_params,
            );
        }
        for function in class.member_ext_funs.values_mut().flatten() {
            project_class_callable(
                index,
                &mut function.signature,
                anonymous,
                &class.type_parameters.type_params,
            );
        }
        for (name, property) in &mut class.declared_props {
            crate::trace_compiler!(
                "signature",
                "project member property owner={:?} name={} declaration={:?} prior={:?} resolved={:?}",
                class.internal,
                name,
                property.stable_declaration,
                property.ty,
                resolved_result(index, property.stable_declaration),
            );
            let Some(mut result) = resolved_result(index, property.stable_declaration) else {
                continue;
            };
            if anonymous {
                result = classifier_relative_type(result, &class.type_parameters.type_params);
            }
            property.ty = result;
            property.storage_ty = property
                .stable_declaration
                .and_then(|declaration| index.property_for_declaration(declaration))
                .and_then(|property| index.property(property))
                .and_then(|property| property.storage_type)
                .map(|storage| storage.get())
                .map(|storage| {
                    if anonymous {
                        classifier_relative_type(storage, &class.type_parameters.type_params)
                    } else {
                        storage
                    }
                })
                .or(property
                    .storage_ty
                    .filter(|storage| *storage != crate::types::Ty::Pending));
            if result.mentions_ty_param() {
                class.generic_property_shapes.insert(name.clone(), result);
            } else {
                class.generic_property_shapes.remove(name);
            }
            if let Some((_, ty, _)) = class
                .props
                .iter_mut()
                .find(|(candidate, _, _)| candidate == name)
            {
                *ty = property.storage_ty.unwrap_or(result);
            }
        }
        for (name, properties) in &mut class.contextual_props {
            for property in properties {
                crate::trace_compiler!(
                    "signature",
                    "project contextual member property owner={:?} name={} declaration={:?} prior={:?} resolved={:?}",
                    class.internal,
                    name,
                    property.stable_declaration,
                    property.ty,
                    resolved_result(index, property.stable_declaration),
                );
                let Some(mut result) = resolved_result(index, property.stable_declaration) else {
                    continue;
                };
                if anonymous {
                    result = classifier_relative_type(result, &class.type_parameters.type_params);
                }
                property.ty = result;
            }
        }
        for property in class.member_ext_props.values_mut().flatten() {
            let Some(declaration) = property.stable_declaration else {
                continue;
            };
            let Some(signature) = index.signature(declaration) else {
                continue;
            };
            property.ret = if anonymous {
                classifier_relative_type(signature.result.get(), &class.type_parameters.type_params)
            } else {
                signature.result.get()
            };
            property.context_params = signature
                .parameters
                .iter()
                .map(|parameter| parameter.get())
                .collect();
            if let Some(receiver) = index
                .property_for_declaration(declaration)
                .and_then(|property| index.property(property))
                .and_then(|property| property.extension_receiver)
            {
                property.receiver = receiver.get();
            }
        }
    }
    table.finish_module_mutation();
}

/// Seal serialization-only declaration facts into the stable Pass-1 product.
///
/// The legacy symbol table still owns source-spelling collection and bounded constant evaluation
/// during this migration. This is its final permitted handoff: Pass 2 and every later phase consume
/// these facts through stable declaration identity and never receive the table itself for metadata.
pub(crate) fn publish_stable_declaration_metadata(
    index: &mut ResolvedModuleIndex,
    table: &SymbolTable,
) {
    let classifier_hierarchies = table
        .classes
        .values()
        .filter_map(|class| {
            class.stable_declaration.and_then(|declaration| {
                // An undemanded ordinary body-local classifier may use lexical aliases unavailable
                // at the module boundary. Its declaration inventory survives Pass 1, but its
                // checked classifier header and applied hierarchy are both published from the same
                // authoritative Pass-2 body unit. Never attach the provisional legacy hierarchy to
                // a declaration whose semantic header was deliberately deferred.
                index.classifier_header(declaration)?;
                Some((
                    declaration,
                    table.applied_hierarchy(crate::types::Ty::obj_name(class.internal)),
                ))
            })
        })
        .collect::<Vec<_>>();
    for (declaration, hierarchy) in classifier_hierarchies {
        index.publish_classifier_hierarchy(declaration, hierarchy);
    }
    for (&declaration, spellings) in &table.stable_declared_spellings {
        index.publish_declaration_spellings(declaration, spellings.clone());
    }
    for (declaration, suppressions) in table.visibility_suppressed_declarations() {
        index.publish_visibility_suppression(
            declaration,
            suppressions.invisible_reference,
            suppressions.invisible_member,
        );
    }
    for property in table.source_props.values() {
        let (Some(declaration), Some(constant)) = (
            property.stable_declaration,
            property.compile_time_constant.as_ref(),
        ) else {
            continue;
        };
        index.publish_compile_time_constant(declaration, constant.clone());
    }
    for class in table.classes.values() {
        for (name, constant) in &class.constants {
            let Some(declaration) = class
                .declared_props
                .get(name)
                .and_then(|property| property.stable_declaration)
            else {
                continue;
            };
            index.publish_compile_time_constant(declaration, constant.clone());
        }
    }
}
