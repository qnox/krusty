//! Publication of exact Kotlin override edges.
//!
//! Providers and the module declaration model are live only in Pass 1. This module performs the
//! declaration pairing there and stores a compact, provider-neutral edge in stable FIR. It never
//! computes a target descriptor or decides whether a particular backend needs a bridge.

use std::collections::{HashMap, HashSet};

use super::SymbolTable;
use crate::fir::{
    DeclarationFlags, ResolvedFunctionOverride, ResolvedFunctionOverrideTarget,
    ResolvedModuleIndex, ResolvedPropertyOverride, ResolvedPropertyOverrideTarget, ResolvedTy,
};
use crate::libraries::{FnKind, FunctionInfo, PropKind, PropertyInfo};
use crate::module_symbols::ModuleSymbols;
use crate::symbol_source::{CompositeSource, SymbolSource};
use crate::types::{Ty, Visibility};

fn declarations_by_target<T, Target>(
    declarations: impl IntoIterator<Item = T>,
    target_of: impl Fn(&T) -> Option<Target>,
) -> HashMap<Target, T>
where
    Target: Eq + std::hash::Hash,
{
    declarations
        .into_iter()
        .filter_map(|declaration| Some((target_of(&declaration)?, declaration)))
        .collect()
}

fn nearest_unique_by<T, Target>(
    mut candidates: Vec<T>,
    rank_of: impl Fn(&T) -> u32,
    target_of: impl Fn(&T) -> Option<Target>,
) -> Option<T>
where
    Target: Eq + std::hash::Hash,
{
    let nearest = candidates.iter().map(&rank_of).min()?;
    candidates.retain(|candidate| rank_of(candidate) == nearest);
    let mut seen = HashSet::new();
    candidates.retain(|candidate| target_of(candidate).is_some_and(|target| seen.insert(target)));
    if candidates.len() != 1 {
        return None;
    }
    candidates.pop()
}

/// Whether the implementation's own declaration hierarchy already carries this interface
/// obligation. In that case any required representation bridge belongs to that owner, not to every
/// subclass that inherits the implementation. This also prevents a subclass bridge from illegally
/// redeclaring a final Java superclass method such as `Enum.describeConstable`.
fn owner_already_has_obligation(
    source: &dyn SymbolSource,
    implementation_owner: crate::types::TypeName,
    obligation_owner: crate::types::TypeName,
) -> bool {
    let mut queue = std::collections::VecDeque::from([Ty::obj_name(implementation_owner)]);
    let mut seen = HashSet::new();
    while let Some(current) = queue.pop_front() {
        let Some(owner) = current.kotlin_class_internal() else {
            continue;
        };
        if !seen.insert(owner) {
            continue;
        }
        if owner == obligation_owner {
            return true;
        }
        queue.extend(crate::symbol_resolver::direct_supertypes(source, current));
    }
    false
}

fn may_supply_inherited_implementation(
    source: &dyn SymbolSource,
    implementation_owner: crate::types::TypeName,
    obligation_owner: crate::types::TypeName,
) -> bool {
    source
        .classifier(implementation_owner)
        .is_some_and(|owner| owner.is_interface())
        || !owner_already_has_obligation(source, implementation_owner, obligation_owner)
}

fn target(
    index: &ResolvedModuleIndex,
    property: &PropertyInfo,
) -> Option<ResolvedPropertyOverrideTarget> {
    property
        .stable_declaration
        .and_then(|declaration| index.property_for_declaration(declaration))
        .map(ResolvedPropertyOverrideTarget::Module)
        .or_else(|| {
            property
                .getter
                .external_identity
                .map(ResolvedPropertyOverrideTarget::External)
        })
}

fn function_target(
    index: &ResolvedModuleIndex,
    function: &FunctionInfo,
) -> Option<ResolvedFunctionOverrideTarget> {
    function
        .stable_declaration
        .and_then(|declaration| index.callable_for_declaration(declaration))
        .map(|callable| ResolvedFunctionOverrideTarget::Module(callable.id))
        .or_else(|| {
            function
                .callable
                .external_identity
                .map(ResolvedFunctionOverrideTarget::External)
        })
}

fn declared_properties(
    source: &dyn crate::symbol_source::SymbolSource,
    receiver: Ty,
    name: &str,
) -> Vec<PropertyInfo> {
    crate::symbol_resolver::declared_member_callables(source, receiver, name)
        .into_parts()
        .1
        .overloads
        .into_iter()
        .filter(|property| {
            property.kind == PropKind::Member
                && property.context_count == 0
                && property.visibility != Visibility::Private
        })
        .collect()
}

fn declared_functions(
    source: &dyn crate::symbol_source::SymbolSource,
    receiver: Ty,
    name: &str,
) -> Vec<FunctionInfo> {
    crate::symbol_resolver::declared_member_callables(source, receiver, name)
        .into_parts()
        .0
        .overloads
        .into_iter()
        .filter(|function| {
            function.kind == FnKind::Member && function.visibility != Visibility::Private
        })
        .collect()
}

fn equivalent(source: &dyn SymbolSource, left: Ty, right: Ty) -> bool {
    let left = left.canonical_semantic();
    let right = right.canonical_semantic();
    left == right
        || crate::symbol_resolver::resolution_subtype(source, left, right)
            && crate::symbol_resolver::resolution_subtype(source, right, left)
}

fn resolved_ty(ty: Ty, what: &str) -> ResolvedTy {
    ResolvedTy::new(ty.canonical_semantic()).unwrap_or_else(|_| panic!("{what} must be finalized"))
}

fn resolved_types(types: impl IntoIterator<Item = Ty>, what: &str) -> Box<[ResolvedTy]> {
    types
        .into_iter()
        .map(|ty| resolved_ty(ty, what))
        .collect::<Vec<_>>()
        .into_boxed_slice()
}

fn declaration_formals(
    index: &ResolvedModuleIndex,
    declaration: crate::fir::DeclarationId,
) -> Vec<String> {
    (0..)
        .map_while(|ordinal| index.type_parameter(declaration, ordinal))
        .filter_map(|parameter| {
            index
                .type_parameter_semantic_name(parameter)
                .map(str::to_owned)
        })
        .collect()
}

fn publish_inherited_interface_function_plans(
    index: &ResolvedModuleIndex,
    source: &dyn crate::symbol_source::SymbolSource,
    implementation_owner: crate::types::TypeName,
    hierarchy: &[crate::fir::ResolvedAppliedClassifier],
    overrides: &mut Vec<ResolvedFunctionOverride>,
) {
    let root = hierarchy
        .iter()
        .find(|entry| entry.depth == 0)
        .map(|entry| entry.applied.get())
        .unwrap_or_else(|| Ty::obj_name(implementation_owner));
    for supertype in hierarchy.iter().filter(|entry| entry.depth != 0) {
        let Some(interface) = source
            .classifier(supertype.classifier)
            .filter(|classifier| classifier.is_interface())
        else {
            continue;
        };
        for name in &interface.declared_callable_order {
            let raw = declarations_by_target(
                declared_functions(source, Ty::obj_name(supertype.classifier), name),
                |function| function_target(index, function),
            );
            for applied in declared_functions(source, supertype.applied.get(), name) {
                let Some(overridden) = function_target(index, &applied) else {
                    continue;
                };
                if overrides.iter().any(|edge| edge.overridden == overridden) {
                    continue;
                }
                let Some(declared) = raw.get(&overridden) else {
                    continue;
                };
                let applied_parameters = applied.semantic_params();
                let applied_result = applied.ret.apply(applied.callable.ret).canonical_semantic();
                let candidates = crate::symbol_resolver::members_in_hierarchy(source, root, name)
                    .functions()
                    .iter()
                    .filter(|candidate| {
                        candidate.visibility != Visibility::Private
                            && !candidate.flags.is_abstract
                            && may_supply_inherited_implementation(
                                source,
                                candidate.callable.owner,
                                supertype.classifier,
                            )
                            && function_target(index, candidate)
                                .is_some_and(|target| target != overridden)
                    })
                    .filter(|candidate| {
                        let parameters = candidate.semantic_params();
                        parameters.len() == applied_parameters.len()
                            && parameters.iter().zip(applied_parameters.iter()).all(
                                |(&implementation, &base)| equivalent(source, implementation, base),
                            )
                            && crate::symbol_resolver::resolution_subtype(
                                source,
                                candidate
                                    .ret
                                    .apply(candidate.callable.ret)
                                    .canonical_semantic(),
                                applied_result,
                            )
                    })
                    .cloned()
                    .collect::<Vec<_>>();
                let Some(implementation) = nearest_unique_by(
                    candidates,
                    |candidate| candidate.receiver_rank,
                    |candidate| function_target(index, candidate),
                ) else {
                    continue;
                };
                let Some(implementation_target) = function_target(index, &implementation) else {
                    continue;
                };
                let implementation_declarations = declarations_by_target(
                    declared_functions(source, Ty::obj_name(implementation.callable.owner), name),
                    |function| function_target(index, function),
                );
                let Some(implementation_declared) =
                    implementation_declarations.get(&implementation_target)
                else {
                    continue;
                };
                let declared_parameters = declared.semantic_params();
                overrides.push(ResolvedFunctionOverride {
                    implementation: implementation_target,
                    implementation_owner: implementation.callable.owner,
                    overridden,
                    overridden_owner: supertype.classifier,
                    overridden_is_interface: true,
                    name: name.clone().into_boxed_str(),
                    declared_parameters: resolved_types(
                        declared_parameters.iter().copied(),
                        "inherited interface declaration parameters",
                    ),
                    declared_result: resolved_ty(
                        declared.ret.apply(declared.callable.ret),
                        "inherited interface result",
                    ),
                    applied_parameters: resolved_types(
                        applied_parameters.iter().copied(),
                        "applied inherited interface parameters",
                    ),
                    applied_result: resolved_ty(
                        applied_result,
                        "applied inherited interface result",
                    ),
                    implementation_parameters: resolved_types(
                        implementation_declared.semantic_params().iter().copied(),
                        "inherited implementation parameters",
                    ),
                    implementation_result: resolved_ty(
                        implementation_declared
                            .ret
                            .apply(implementation_declared.callable.ret),
                        "inherited implementation result",
                    ),
                    suspend: implementation.flags.suspend,
                    depth: supertype.depth,
                });
            }
        }
    }
}

fn publish_inherited_interface_property_plans(
    index: &ResolvedModuleIndex,
    source: &dyn crate::symbol_source::SymbolSource,
    implementation_owner: crate::types::TypeName,
    hierarchy: &[crate::fir::ResolvedAppliedClassifier],
    overrides: &mut Vec<ResolvedPropertyOverride>,
) {
    let root = hierarchy
        .iter()
        .find(|entry| entry.depth == 0)
        .map(|entry| entry.applied.get())
        .unwrap_or_else(|| Ty::obj_name(implementation_owner));
    for supertype in hierarchy.iter().filter(|entry| entry.depth != 0) {
        let Some(interface) = source
            .classifier(supertype.classifier)
            .filter(|classifier| classifier.is_interface())
        else {
            continue;
        };
        for name in &interface.declared_callable_order {
            let raw = declarations_by_target(
                declared_properties(source, Ty::obj_name(supertype.classifier), name),
                |property| target(index, property),
            );
            for applied in declared_properties(source, supertype.applied.get(), name) {
                let Some(overridden) = target(index, &applied) else {
                    continue;
                };
                if overrides.iter().any(|edge| edge.overridden == overridden) {
                    continue;
                }
                let Some(declared) = raw.get(&overridden) else {
                    continue;
                };
                let candidates = crate::symbol_resolver::members_in_hierarchy(source, root, name)
                    .properties()
                    .iter()
                    .filter(|candidate| {
                        candidate.visibility != Visibility::Private
                            && !candidate.getter.is_abstract
                            && may_supply_inherited_implementation(
                                source,
                                candidate.owner,
                                supertype.classifier,
                            )
                            && target(index, candidate)
                                .is_some_and(|implementation| implementation != overridden)
                    })
                    .filter(|candidate| {
                        if applied.setter.is_some() {
                            candidate.setter.is_some()
                                && candidate.ty.canonical_semantic()
                                    == applied.ty.canonical_semantic()
                        } else {
                            crate::symbol_resolver::resolution_subtype(
                                source,
                                candidate.ty.canonical_semantic(),
                                applied.ty.canonical_semantic(),
                            )
                        }
                    })
                    .cloned()
                    .collect::<Vec<_>>();
                let Some(implementation) = nearest_unique_by(
                    candidates,
                    |candidate| candidate.receiver_rank,
                    |candidate| target(index, candidate),
                ) else {
                    continue;
                };
                let Some(implementation_target) = target(index, &implementation) else {
                    continue;
                };
                let implementation_declarations = declarations_by_target(
                    declared_properties(source, Ty::obj_name(implementation.owner), name),
                    |property| target(index, property),
                );
                let Some(implementation_declared) =
                    implementation_declarations.get(&implementation_target)
                else {
                    continue;
                };
                overrides.push(ResolvedPropertyOverride {
                    implementation: implementation_target,
                    implementation_owner: implementation.owner,
                    overridden,
                    overridden_owner: supertype.classifier,
                    overridden_is_interface: true,
                    name: name.clone().into_boxed_str(),
                    declared_type: resolved_ty(declared.ty, "inherited interface property type"),
                    applied_type: resolved_ty(
                        applied.ty,
                        "applied inherited interface property type",
                    ),
                    implementation_type: resolved_ty(
                        implementation_declared.ty,
                        "inherited implementation property type",
                    ),
                    overridden_mutable: applied.setter.is_some(),
                    implementation_mutable: implementation.setter.is_some(),
                    depth: supertype.depth,
                });
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn append_property_override_edges(
    index: &ResolvedModuleIndex,
    source: &dyn SymbolSource,
    implementation_owner: crate::types::TypeName,
    name: &str,
    implementation_id: crate::fir::PropertyId,
    implementation_type: ResolvedTy,
    implementation_mutable: bool,
    hierarchy: &[crate::fir::ResolvedAppliedClassifier],
    seen: &mut HashSet<ResolvedPropertyOverrideTarget>,
    overrides: &mut Vec<ResolvedPropertyOverride>,
) {
    for supertype in hierarchy.iter().filter(|entry| entry.depth != 0) {
        let overridden_is_interface = source
            .classifier(supertype.classifier)
            .is_some_and(|classifier| classifier.is_interface());
        let raw = declarations_by_target(
            declared_properties(source, Ty::obj_name(supertype.classifier), name),
            |property| target(index, property),
        );
        for applied in declared_properties(source, supertype.applied.get(), name) {
            let Some(overridden) = target(index, &applied) else {
                continue;
            };
            let Some(declared) = raw.get(&overridden) else {
                continue;
            };
            let compatible_type = if applied.setter.is_some() {
                applied.ty.canonical_semantic() == implementation_type.get().canonical_semantic()
            } else {
                crate::symbol_resolver::resolution_subtype(
                    source,
                    implementation_type.get().canonical_semantic(),
                    applied.ty.canonical_semantic(),
                )
            };
            if !compatible_type
                || applied.setter.is_some() && !implementation_mutable
                || !seen.insert(overridden)
            {
                continue;
            }
            overrides.push(ResolvedPropertyOverride {
                implementation: ResolvedPropertyOverrideTarget::Module(implementation_id),
                implementation_owner,
                overridden,
                overridden_owner: supertype.classifier,
                overridden_is_interface,
                name: name.into(),
                declared_type: resolved_ty(
                    declared.ty,
                    "provider declaration entering stable override FIR",
                ),
                applied_type: resolved_ty(applied.ty, "applied override type entering stable FIR"),
                implementation_type,
                overridden_mutable: applied.setter.is_some(),
                implementation_mutable,
                depth: supertype.depth,
            });
        }
    }
}

fn property_override_plans(
    index: &ResolvedModuleIndex,
    source: &dyn SymbolSource,
    class: &super::ClassSig,
    hierarchy: &[crate::fir::ResolvedAppliedClassifier],
) -> Vec<ResolvedPropertyOverride> {
    let mut overrides = Vec::new();
    let mut seen = HashSet::new();
    for (name, implementation) in &class.declared_props {
        let Some(declaration) = implementation.stable_declaration else {
            continue;
        };
        let Some(header) = index.declaration_header(declaration) else {
            continue;
        };
        if !header.flags.has(DeclarationFlags::OVERRIDE) {
            continue;
        }
        let Some(implementation_id) = index.property_for_declaration(declaration) else {
            continue;
        };
        let Some(implementation_type) = index.signature(declaration).and_then(|signature| {
            ResolvedTy::new(signature.result.get().canonical_semantic()).ok()
        }) else {
            continue;
        };
        append_property_override_edges(
            index,
            source,
            class.internal_name(),
            name,
            implementation_id,
            implementation_type,
            implementation.setter_name.is_some(),
            hierarchy,
            &mut seen,
            &mut overrides,
        );
    }
    publish_inherited_interface_property_plans(
        index,
        source,
        class.internal_name(),
        hierarchy,
        &mut overrides,
    );
    overrides.sort_by_key(|edge| edge.depth);
    overrides
}

#[allow(clippy::too_many_arguments)]
fn append_function_override_edges(
    index: &ResolvedModuleIndex,
    source: &dyn SymbolSource,
    implementation_owner: crate::types::TypeName,
    name: &str,
    implementation_callable: crate::fir::CallableId,
    implementation_formals: &[String],
    implementation_parameters: &[Ty],
    implementation_result: Ty,
    suspend: bool,
    hierarchy: &[crate::fir::ResolvedAppliedClassifier],
    seen: &mut HashSet<ResolvedFunctionOverrideTarget>,
    overrides: &mut Vec<ResolvedFunctionOverride>,
) {
    for supertype in hierarchy.iter().filter(|entry| entry.depth != 0) {
        let overridden_is_interface = source
            .classifier(supertype.classifier)
            .is_some_and(|classifier| classifier.is_interface());
        let raw = declarations_by_target(
            declared_functions(source, Ty::obj_name(supertype.classifier), name),
            |function| function_target(index, function),
        );
        for applied in declared_functions(source, supertype.applied.get(), name) {
            let Some(overridden) = function_target(index, &applied) else {
                continue;
            };
            let Some(declared) = raw.get(&overridden) else {
                continue;
            };
            let applied_parameters = applied.semantic_params();
            let applied_formals = applied
                .generic_sig
                .as_ref()
                .map(|signature| signature.formals.as_slice())
                .unwrap_or_default();
            if applied_parameters.len() != implementation_parameters.len()
                || !applied_parameters
                    .iter()
                    .zip(implementation_parameters)
                    .all(|(&base, &implementation)| {
                        equivalent(
                            source,
                            crate::types::ty_canonicalize_params(base, applied_formals),
                            crate::types::ty_canonicalize_params(
                                implementation,
                                implementation_formals,
                            ),
                        )
                    })
                || !crate::symbol_resolver::resolution_subtype(
                    source,
                    crate::types::ty_canonicalize_params(
                        implementation_result,
                        implementation_formals,
                    ),
                    crate::types::ty_canonicalize_params(
                        applied.ret.apply(applied.callable.ret).canonical_semantic(),
                        applied_formals,
                    ),
                )
                || !seen.insert(overridden)
            {
                continue;
            }
            let declared_parameters = declared.semantic_params();
            overrides.push(ResolvedFunctionOverride {
                implementation: ResolvedFunctionOverrideTarget::Module(implementation_callable),
                implementation_owner,
                overridden,
                overridden_owner: supertype.classifier,
                overridden_is_interface,
                name: name.into(),
                declared_parameters: resolved_types(
                    declared_parameters.iter().copied(),
                    "overridden function declaration parameters",
                ),
                declared_result: resolved_ty(
                    declared.ret.apply(declared.callable.ret),
                    "overridden function result",
                ),
                applied_parameters: resolved_types(
                    applied_parameters.iter().copied(),
                    "applied overridden function parameters",
                ),
                applied_result: resolved_ty(
                    applied.ret.apply(applied.callable.ret),
                    "applied overridden function result",
                ),
                implementation_parameters: resolved_types(
                    implementation_parameters.iter().copied(),
                    "overriding function parameters",
                ),
                implementation_result: resolved_ty(
                    implementation_result,
                    "overriding function result",
                ),
                suspend,
                depth: supertype.depth,
            });
        }
    }
}

fn function_override_plans(
    index: &ResolvedModuleIndex,
    source: &dyn SymbolSource,
    class: &super::ClassSig,
    hierarchy: &[crate::fir::ResolvedAppliedClassifier],
) -> Vec<ResolvedFunctionOverride> {
    let mut overrides = Vec::new();
    for (name, implementations) in &class.methods {
        for implementation in implementations {
            if !implementation.is_override() {
                continue;
            }
            let Some(declaration) = implementation.stable_declaration else {
                continue;
            };
            let Some(implementation_callable) = index.callable_for_declaration(declaration) else {
                continue;
            };
            let Some(implementation_signature) = index.signature(declaration) else {
                continue;
            };
            let implementation_parameters = implementation_signature
                .parameters
                .iter()
                .map(|parameter| parameter.get())
                .collect::<Vec<_>>();
            let implementation_result = implementation_signature.result.get().canonical_semantic();
            let implementation_formals = declaration_formals(index, declaration);
            let mut seen = HashSet::new();
            append_function_override_edges(
                index,
                source,
                class.internal_name(),
                name,
                implementation_callable.id,
                &implementation_formals,
                &implementation_parameters,
                implementation_result,
                implementation.is_suspend(),
                hierarchy,
                &mut seen,
                &mut overrides,
            );
        }
    }
    publish_inherited_interface_function_plans(
        index,
        source,
        class.internal_name(),
        hierarchy,
        &mut overrides,
    );
    overrides.sort_by_key(|edge| edge.depth);
    overrides
}

/// Complete the semantic override edges of body-local classifiers after their checked Pass-2
/// signatures have been published. The matcher is the same one used for module declarations; the
/// only difference is timing. No parser coordinate, target descriptor, or backend spelling enters
/// the retained plan.
pub(crate) fn publish_checked_local_override_plans(
    index: &mut ResolvedModuleIndex,
    platform: &dyn crate::libraries::SemanticPlatform,
    source_file: u32,
    classifiers: &[crate::fir::DeclarationId],
) {
    let plans = {
        let module = crate::fir::StreamedModuleSymbols::for_file(index, source_file);
        let source = CompositeSource::new(vec![
            &module as &dyn SymbolSource,
            platform as &dyn SymbolSource,
        ]);
        classifiers
            .iter()
            .copied()
            .filter_map(|classifier| {
                let header = index.declaration_header(classifier)?;
                if !header.flags.has(DeclarationFlags::LOCAL_CLASS) {
                    return None;
                }
                let implementation_owner = index.classifier_header(classifier)?.classifier;
                let hierarchy = index.classifier_hierarchy(classifier)?.to_vec();
                let mut properties = Vec::new();
                let mut property_seen = HashSet::new();
                let mut functions = Vec::new();
                for raw in 0..index.declaration_count() {
                    let declaration = crate::fir::DeclarationId::from_raw(raw as u32);
                    let Some(member) = index
                        .declaration_header(declaration)
                        .filter(|member| member.owner == Some(classifier))
                    else {
                        continue;
                    };
                    if !member.flags.has(DeclarationFlags::OVERRIDE) {
                        continue;
                    }
                    let Some(name) = index.declaration_name(declaration) else {
                        continue;
                    };
                    let Some(signature) = index.signature(declaration) else {
                        continue;
                    };
                    match member.kind {
                        crate::fir::DeclarationKind::Property => {
                            let Some(property) = index.property_for_declaration(declaration) else {
                                continue;
                            };
                            let Some(property_header) = index.property(property) else {
                                continue;
                            };
                            append_property_override_edges(
                                index,
                                &source,
                                implementation_owner,
                                name,
                                property,
                                signature.result,
                                property_header.mutable,
                                &hierarchy,
                                &mut property_seen,
                                &mut properties,
                            );
                        }
                        crate::fir::DeclarationKind::Function => {
                            let Some(callable) = index.callable_for_declaration(declaration) else {
                                continue;
                            };
                            let parameters = signature
                                .parameters
                                .iter()
                                .map(|parameter| parameter.get())
                                .collect::<Vec<_>>();
                            let mut seen = HashSet::new();
                            let implementation_formals = declaration_formals(index, declaration);
                            append_function_override_edges(
                                index,
                                &source,
                                implementation_owner,
                                name,
                                callable.id,
                                &implementation_formals,
                                &parameters,
                                signature.result.get().canonical_semantic(),
                                member.flags.has(DeclarationFlags::SUSPEND),
                                &hierarchy,
                                &mut seen,
                                &mut functions,
                            );
                        }
                        crate::fir::DeclarationKind::Classifier
                        | crate::fir::DeclarationKind::EnumEntry
                        | crate::fir::DeclarationKind::TypeAlias
                        | crate::fir::DeclarationKind::Constructor
                        | crate::fir::DeclarationKind::Accessor
                        | crate::fir::DeclarationKind::Initializer
                        | crate::fir::DeclarationKind::Script => {}
                    }
                }
                publish_inherited_interface_property_plans(
                    index,
                    &source,
                    implementation_owner,
                    &hierarchy,
                    &mut properties,
                );
                publish_inherited_interface_function_plans(
                    index,
                    &source,
                    implementation_owner,
                    &hierarchy,
                    &mut functions,
                );
                properties.sort_by_key(|edge| edge.depth);
                functions.sort_by_key(|edge| edge.depth);
                Some((classifier, properties, functions))
            })
            .collect::<Vec<_>>()
    };
    for (classifier, properties, functions) in plans {
        if !index.has_property_override_plan(classifier) {
            index.publish_property_overrides(classifier, properties);
        }
        if !index.has_function_override_plan(classifier) {
            index.publish_function_overrides(classifier, functions);
        }
    }
}

pub(crate) fn publish_override_plans(index: &mut ResolvedModuleIndex, table: &SymbolTable) {
    let module = ModuleSymbols::new(table);
    let source = CompositeSource::new(vec![&module as &dyn SymbolSource, table.libraries.as_ref()]);
    let classifiers = table
        .classes
        .values()
        .filter_map(|class| {
            class.stable_declaration.and_then(|declaration| {
                // A body-local classifier whose semantic header is deferred to Pass 2 cannot own
                // a Pass-1 override plan. Its checked lexical publication must supply the hierarchy
                // and override identities together; publishing an empty provisional plan here would
                // falsely make that absence final.
                index
                    .classifier_header(declaration)
                    .filter(|_| {
                        !index
                            .declaration_header(declaration)
                            .is_some_and(|header| header.flags.has(DeclarationFlags::LOCAL_CLASS))
                    })
                    .map(|_| (declaration, class))
            })
        })
        .collect::<Vec<_>>();

    for (classifier, class) in classifiers {
        let hierarchy = index
            .classifier_hierarchy(classifier)
            .unwrap_or_default()
            .to_vec();
        let properties = property_override_plans(index, &source, class, &hierarchy);
        let functions = function_override_plans(index, &source, class, &hierarchy);
        index.publish_property_overrides(classifier, properties);
        index.publish_function_overrides(classifier, functions);
    }
}
