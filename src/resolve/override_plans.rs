//! Publication of exact Kotlin override edges.
//!
//! Providers and the module declaration model are live only in Pass 1. This module performs the
//! declaration pairing there and stores a compact, provider-neutral edge in stable FIR. It never
//! computes a target descriptor or decides whether a particular backend needs a bridge.

use std::collections::{HashMap, HashSet};

use super::SymbolTable;
use crate::fir::{
    DeclarationFlags, DeclarationId, DeclarationKind, ResolvedAppliedClassifier,
    ResolvedFunctionOverride, ResolvedFunctionOverrideTarget, ResolvedModuleIndex,
    ResolvedPropertyOverride, ResolvedPropertyOverrideTarget, ResolvedTy,
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

/// For each overridden declaration of ONE implementation, the other overridden declarations that
/// belong to a superclass (not an interface) which inherits the first one's owner. Such a
/// superclass member itself overrides that declaration, so the declaration reaches the
/// implementation through the superclass chain rather than first at the implementation.
fn has_kotlin_superclass_override(
    source: &dyn SymbolSource,
    overridden: impl Iterator<Item = (crate::types::TypeName, bool)>,
) -> Vec<bool> {
    let overridden = overridden.collect::<Vec<_>>();
    overridden
        .iter()
        .map(|&(owner, _)| {
            overridden
                .iter()
                .any(|&(superclass, superclass_is_interface)| {
                    !superclass_is_interface
                        && superclass != owner
                        && owner_already_has_obligation(source, superclass, owner)
                        && source
                            .classifier(superclass)
                            .unwrap_or_else(|| {
                                panic!("override owner {superclass} must remain resolvable")
                            })
                            .is_kotlin
                })
        })
        .collect()
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

/// The exact declaration `property` selects and its unsubstituted declared type: the signature a
/// generated implementation (an interface delegation forwarder) must bridge to.
pub(super) fn property_declaration(
    index: &ResolvedModuleIndex,
    property: &PropertyInfo,
) -> Option<(ResolvedPropertyOverrideTarget, Ty)> {
    let selected = target(index, property)?;
    let declared = match selected {
        ResolvedPropertyOverrideTarget::Module(_) => {
            index.signature(property.stable_declaration?)?.result.get()
        }
        ResolvedPropertyOverrideTarget::External(_) => {
            let getter = &property.getter;
            getter
                .declared_ret
                .or_else(|| getter.generic_sig.as_ref().map(|signature| signature.ret))
                .unwrap_or(getter.ret)
        }
    };
    Some((selected, declared))
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

fn function_default_provider(
    index: &ResolvedModuleIndex,
    function: &FunctionInfo,
    target: ResolvedFunctionOverrideTarget,
) -> Option<ResolvedFunctionOverrideTarget> {
    if !function
        .call_sig
        .param_defaults
        .iter()
        .any(|default| *default)
    {
        return None;
    }
    if let Some(provider) = function.callable.external_default_provider {
        return Some(ResolvedFunctionOverrideTarget::External(provider));
    }
    Some(match target {
        ResolvedFunctionOverrideTarget::Module(callable) => index
            .callable_default_provider(callable)
            .unwrap_or(ResolvedFunctionOverrideTarget::Module(callable)),
        ResolvedFunctionOverrideTarget::External(callable) => {
            ResolvedFunctionOverrideTarget::External(callable)
        }
    })
}

fn function_default_bitmap(function: &FunctionInfo) -> Box<[bool]> {
    let count = function.semantic_params().len();
    (0..count)
        .map(|parameter| {
            function
                .call_sig
                .param_defaults
                .get(parameter)
                .copied()
                .unwrap_or(false)
        })
        .collect()
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
            matches!(function.kind, FnKind::Member | FnKind::Extension)
                && function.visibility != Visibility::Private
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

fn module_parameter_identities(
    index: &ResolvedModuleIndex,
    callable: crate::fir::CallableId,
    count: usize,
) -> Box<[crate::fir::ResolvedParameterIdentity]> {
    index
        .callable_parameter_identities(callable, count)
        .expect("an override implementation must publish every typed parameter identity")
}

fn declaration_parameters_with_receiver(function: &crate::libraries::FunctionInfo) -> Vec<Ty> {
    let mut parameters = function.semantic_params().into_owned();
    if let Some(receiver) = function
        .semantic_receiver()
        .filter(|_| function.is_extension())
    {
        parameters.insert(function.context_count.min(parameters.len()), receiver);
    }
    parameters
}

fn applied_parameters_with_receiver(function: &crate::libraries::FunctionInfo) -> Vec<Ty> {
    declaration_parameters_with_receiver(function)
}

fn function_parameter_identities(
    function: &crate::libraries::FunctionInfo,
    count: usize,
) -> Box<[crate::fir::ResolvedParameterIdentity]> {
    let extension_position = (function.is_extension()
        && count == function.call_sig.parameter_identities.len() + 1)
        .then_some(function.context_count);
    function
        .call_sig
        .physical_parameter_identities(count, function.context_count, extension_position)
        .expect("a normalized override declaration publishes every typed parameter identity")
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
                let applied_parameters = applied_parameters_with_receiver(&applied);
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
                        let parameters = applied_parameters_with_receiver(candidate);
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
                let declared_parameters = declaration_parameters_with_receiver(declared);
                let implementation_parameters =
                    declaration_parameters_with_receiver(implementation_declared);
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
                        implementation_parameters.iter().copied(),
                        "inherited implementation parameters",
                    ),
                    implementation_parameter_identities: function_parameter_identities(
                        &implementation,
                        implementation_parameters.len(),
                    ),
                    implementation_result: resolved_ty(
                        implementation_declared
                            .ret
                            .apply(implementation_declared.callable.ret),
                        "inherited implementation result",
                    ),
                    overridden_parameter_defaults: function_default_bitmap(&applied),
                    overridden_default_provider: function_default_provider(
                        index, &applied, overridden,
                    ),
                    suspend: implementation.flags.suspend,
                    has_kotlin_superclass_override: false,
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
                    has_kotlin_superclass_override: false,
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
    let first = overrides.len();
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
                has_kotlin_superclass_override: false,
                depth: supertype.depth,
            });
        }
    }
    let edges = &mut overrides[first..];
    let covering = has_kotlin_superclass_override(
        source,
        edges
            .iter()
            .map(|edge| (edge.overridden_owner, edge.overridden_is_interface)),
    );
    for (edge, covered) in edges.iter_mut().zip(covering) {
        edge.has_kotlin_superclass_override = covered;
    }
}

/// `names` without duplicates, in the class's lexical declaration order. A name the source did not
/// declare (a synthesized member) follows the declared ones, in name order.
fn declaration_ordered<'a>(
    class: &super::ClassSig,
    names: impl Iterator<Item = &'a String>,
) -> Vec<&'a String> {
    let mut names: Vec<_> = names.collect();
    names.sort_by_key(|name| {
        let position = class
            .declared_callable_order
            .iter()
            .position(|declared| declared == *name);
        (position.unwrap_or(usize::MAX), *name)
    });
    names.dedup();
    names
}

fn property_override_plans(
    index: &ResolvedModuleIndex,
    source: &dyn SymbolSource,
    class: &super::ClassSig,
    hierarchy: &[crate::fir::ResolvedAppliedClassifier],
) -> Vec<ResolvedPropertyOverride> {
    let mut overrides = Vec::new();
    let mut seen = HashSet::new();
    let declared = declaration_ordered(class, class.declared_props.keys())
        .into_iter()
        .map(|name| (name, &class.declared_props[name]));
    for (name, implementation) in declared {
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
    implementation_receiver: Option<Ty>,
    implementation_context_count: usize,
    implementation_result: Ty,
    suspend: bool,
    hierarchy: &[crate::fir::ResolvedAppliedClassifier],
    seen: &mut HashSet<ResolvedFunctionOverrideTarget>,
    overrides: &mut Vec<ResolvedFunctionOverride>,
) {
    let first = overrides.len();
    let mut implementation_inputs = implementation_parameters.to_vec();
    if implementation_receiver.is_some() {
        assert!(
            implementation_context_count < implementation_inputs.len(),
            "an extension override must retain its declared receiver parameter"
        );
        implementation_inputs.remove(implementation_context_count);
    }
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
            let applied_parameters = applied_parameters_with_receiver(&applied);
            let applied_inputs = applied.semantic_params();
            let applied_formals = applied
                .generic_sig
                .as_ref()
                .map(|signature| signature.formals.as_slice())
                .unwrap_or_default();
            if !crate::symbol_resolver::override_input_shapes_match(
                source,
                crate::symbol_resolver::OverrideInputShape {
                    params: &applied_inputs,
                    receiver: applied
                        .semantic_receiver()
                        .filter(|_| applied.is_extension()),
                    formals: applied_formals,
                    context_count: applied.context_count,
                    suspend: applied.flags.suspend,
                },
                crate::symbol_resolver::OverrideInputShape {
                    params: &implementation_inputs,
                    receiver: implementation_receiver,
                    formals: implementation_formals,
                    context_count: implementation_context_count,
                    suspend,
                },
            ) || !crate::symbol_resolver::resolution_subtype(
                source,
                crate::types::ty_canonicalize_params(implementation_result, implementation_formals),
                crate::types::ty_canonicalize_params(
                    applied.ret.apply(applied.callable.ret).canonical_semantic(),
                    applied_formals,
                ),
            ) || !seen.insert(overridden)
            {
                continue;
            }
            let declared_parameters = declaration_parameters_with_receiver(declared);
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
                implementation_parameter_identities: module_parameter_identities(
                    index,
                    implementation_callable,
                    implementation_parameters.len(),
                ),
                implementation_result: resolved_ty(
                    implementation_result,
                    "overriding function result",
                ),
                overridden_parameter_defaults: function_default_bitmap(&applied),
                overridden_default_provider: function_default_provider(index, &applied, overridden),
                suspend,
                has_kotlin_superclass_override: false,
                depth: supertype.depth,
            });
        }
    }
    let edges = &mut overrides[first..];
    let covering = has_kotlin_superclass_override(
        source,
        edges
            .iter()
            .map(|edge| (edge.overridden_owner, edge.overridden_is_interface)),
    );
    for (edge, covered) in edges.iter_mut().zip(covering) {
        edge.has_kotlin_superclass_override = covered;
    }
}

fn function_override_plans(
    index: &ResolvedModuleIndex,
    source: &dyn SymbolSource,
    class: &super::ClassSig,
    hierarchy: &[crate::fir::ResolvedAppliedClassifier],
) -> Vec<ResolvedFunctionOverride> {
    let mut overrides = Vec::new();
    let mut append_implementation = |name: &str, implementation: &super::Signature| {
        if !implementation.is_override() {
            return;
        }
        let Some(declaration) = implementation.stable_declaration else {
            return;
        };
        let Some(implementation_callable) = index.callable_for_declaration(declaration) else {
            return;
        };
        let Some(implementation_signature) = index.signature(declaration) else {
            return;
        };
        let mut implementation_parameters = implementation_signature
            .parameters
            .iter()
            .map(|parameter| parameter.get())
            .collect::<Vec<_>>();
        if let Some(receiver) = implementation_callable.shape.extension_receiver {
            implementation_parameters.insert(
                (implementation_callable.shape.context_parameter_count as usize)
                    .min(implementation_parameters.len()),
                receiver.get(),
            );
        }
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
            implementation_callable
                .shape
                .extension_receiver
                .map(crate::fir::ResolvedTy::get),
            implementation_callable.shape.context_parameter_count as usize,
            implementation_result,
            implementation.is_suspend(),
            hierarchy,
            &mut seen,
            &mut overrides,
        );
    };
    // Declaration order, not map order: the plans become bridge methods, and their order is
    // part of the emitted class.
    for name in declaration_ordered(
        class,
        class.methods.keys().chain(class.member_ext_funs.keys()),
    ) {
        for implementation in class.methods.get(name).into_iter().flatten() {
            append_implementation(name, implementation);
        }
        for implementation in class.member_ext_funs.get(name).into_iter().flatten() {
            append_implementation(name, implementation.signature());
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

/// Enum-entry bodies are anonymous subclasses semantically owned by the entry declaration rather
/// than ordinary classifier headers. Their override decisions must nevertheless be frozen in Pass 1:
/// a backend may need the exact erased super declaration to realize a bridge, and cannot rediscover
/// that edge from a generated subclass name or descriptor.
fn enum_entry_override_plans(
    index: &ResolvedModuleIndex,
    source: &dyn SymbolSource,
) -> Vec<(
    DeclarationId,
    Vec<ResolvedPropertyOverride>,
    Vec<ResolvedFunctionOverride>,
)> {
    let mut plans = Vec::new();
    for raw in 0..index.declaration_count() {
        let entry = DeclarationId::from_raw(raw as u32);
        let Some(entry_header) = index
            .declaration_header(entry)
            .filter(|header| header.kind == DeclarationKind::EnumEntry)
        else {
            continue;
        };
        let Some(parent) = entry_header.owner else {
            continue;
        };
        let Some(parent_header) = index.classifier_header(parent) else {
            continue;
        };
        let Some(entry_name) = index.declaration_name(entry) else {
            continue;
        };
        let implementation_owner = parent_header.classifier.nested_child(entry_name);
        // The entry subclass directly extends the enum. Shift the enum's already-applied hierarchy
        // down one rung so matching sees both enum-declared abstract members and its interfaces.
        let hierarchy = index
            .classifier_hierarchy(parent)
            .unwrap_or_default()
            .iter()
            .map(|supertype| ResolvedAppliedClassifier {
                classifier: supertype.classifier,
                applied: supertype.applied,
                depth: supertype.depth.saturating_add(1),
            })
            .collect::<Vec<_>>();
        let mut properties = Vec::new();
        let mut property_seen = HashSet::new();
        let mut functions = Vec::new();
        for member_raw in 0..index.declaration_count() {
            let member = DeclarationId::from_raw(member_raw as u32);
            let Some(header) = index
                .declaration_header(member)
                .filter(|header| header.owner == Some(entry))
            else {
                continue;
            };
            if !header.flags.has(DeclarationFlags::OVERRIDE) {
                continue;
            }
            let Some(name) = index.declaration_name(member) else {
                continue;
            };
            let Some(signature) = index.signature(member) else {
                continue;
            };
            match header.kind {
                DeclarationKind::Property => {
                    let Some(property) = index.property_for_declaration(member) else {
                        continue;
                    };
                    let Some(property_header) = index.property(property) else {
                        continue;
                    };
                    append_property_override_edges(
                        index,
                        source,
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
                DeclarationKind::Function => {
                    let Some(callable) = index.callable_for_declaration(member) else {
                        continue;
                    };
                    let mut parameters = signature
                        .parameters
                        .iter()
                        .map(|parameter| parameter.get())
                        .collect::<Vec<_>>();
                    if let Some(receiver) = callable.shape.extension_receiver {
                        parameters.insert(
                            (callable.shape.context_parameter_count as usize).min(parameters.len()),
                            receiver.get(),
                        );
                    }
                    let formals = declaration_formals(index, member);
                    let mut seen = HashSet::new();
                    append_function_override_edges(
                        index,
                        source,
                        implementation_owner,
                        name,
                        callable.id,
                        &formals,
                        &parameters,
                        callable
                            .shape
                            .extension_receiver
                            .map(crate::fir::ResolvedTy::get),
                        callable.shape.context_parameter_count as usize,
                        signature.result.get().canonical_semantic(),
                        header.flags.has(DeclarationFlags::SUSPEND),
                        &hierarchy,
                        &mut seen,
                        &mut functions,
                    );
                }
                DeclarationKind::Classifier
                | DeclarationKind::EnumEntry
                | DeclarationKind::TypeAlias
                | DeclarationKind::Constructor
                | DeclarationKind::Accessor
                | DeclarationKind::Initializer
                | DeclarationKind::Script => {}
            }
        }
        properties.sort_by_key(|edge| edge.depth);
        functions.sort_by_key(|edge| edge.depth);
        plans.push((entry, properties, functions));
    }
    plans
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
                            let mut parameters = signature
                                .parameters
                                .iter()
                                .map(|parameter| parameter.get())
                                .collect::<Vec<_>>();
                            if let Some(receiver) = callable.shape.extension_receiver {
                                parameters.insert(
                                    (callable.shape.context_parameter_count as usize)
                                        .min(parameters.len()),
                                    receiver.get(),
                                );
                            }
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
                                callable
                                    .shape
                                    .extension_receiver
                                    .map(crate::fir::ResolvedTy::get),
                                callable.shape.context_parameter_count as usize,
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
    let function_edges = plans
        .iter()
        .flat_map(|(_, _, functions)| functions.iter().cloned())
        .collect::<Vec<_>>();
    publish_inherited_function_defaults(index, &function_edges);

    for (classifier, properties, functions) in plans {
        if !index.has_property_override_plan(classifier) {
            index.publish_property_overrides(classifier, properties);
        }
        if !index.has_function_override_plan(classifier) {
            index.publish_function_overrides(classifier, functions);
        }
    }
}

/// Freeze every override edge before publishing an inherited default. Declaration and classifier
/// source order are not topological: a derived classifier may be visited before its base. Resolve
/// the complete graph to a deterministic fixpoint, comparing final provider identity so a diamond
/// through two intermediates that both inherit the same declaration remains unambiguous.
#[derive(Clone, Debug, Eq, PartialEq)]
enum InheritedDefaultState {
    Absent,
    Unique(Vec<bool>, ResolvedFunctionOverrideTarget),
    Ambiguous,
}

fn publish_inherited_function_defaults(
    index: &mut ResolvedModuleIndex,
    function_edges: &[ResolvedFunctionOverride],
) {
    let mut implementations = function_edges
        .iter()
        .filter_map(|edge| match edge.implementation {
            ResolvedFunctionOverrideTarget::Module(callable) => Some(callable),
            ResolvedFunctionOverrideTarget::External(_) => None,
        })
        .collect::<Vec<_>>();
    implementations.sort_unstable_by_key(|callable| callable.raw());
    implementations.dedup();
    let implementation_set = implementations.iter().copied().collect::<HashSet<_>>();
    let module_state = |callable| {
        let defaults = index
            .callable_default_bitmap(callable)
            .expect("an override target must retain its callable parameter inventory");
        if defaults.iter().any(|default| *default) {
            InheritedDefaultState::Unique(
                defaults,
                index
                    .callable_default_provider(callable)
                    .unwrap_or(ResolvedFunctionOverrideTarget::Module(callable)),
            )
        } else {
            InheritedDefaultState::Absent
        }
    };
    let mut states = implementations
        .iter()
        .filter_map(|&callable| match module_state(callable) {
            state @ InheritedDefaultState::Unique(..) => Some((callable, state)),
            InheritedDefaultState::Absent => None,
            InheritedDefaultState::Ambiguous => unreachable!(),
        })
        .collect::<HashMap<_, _>>();
    loop {
        let mut additions = Vec::new();
        for &implementation in &implementations {
            if states.contains_key(&implementation) {
                continue;
            }
            let edges = function_edges
                .iter()
                .filter(|edge| {
                    edge.implementation == ResolvedFunctionOverrideTarget::Module(implementation)
                })
                .collect::<Vec<_>>();
            let Some(nearest) = edges.iter().map(|edge| edge.depth).min() else {
                continue;
            };
            let mut candidates = Vec::new();
            let mut unresolved = false;
            for edge in edges.into_iter().filter(|edge| edge.depth == nearest) {
                let state = match edge.overridden {
                    ResolvedFunctionOverrideTarget::Module(overridden) => {
                        if let Some(state) = states.get(&overridden) {
                            state.clone()
                        } else if implementation_set.contains(&overridden) {
                            unresolved = true;
                            break;
                        } else {
                            module_state(overridden)
                        }
                    }
                    ResolvedFunctionOverrideTarget::External(_) => {
                        if edge
                            .overridden_parameter_defaults
                            .iter()
                            .any(|default| *default)
                        {
                            InheritedDefaultState::Unique(
                                edge.overridden_parameter_defaults.to_vec(),
                                edge.overridden_default_provider.expect(
                                    "an external callable with defaults must retain its provider identity",
                                ),
                            )
                        } else {
                            InheritedDefaultState::Absent
                        }
                    }
                };
                candidates.push(state);
            }
            if unresolved {
                continue;
            }
            let mut unique = candidates.iter().filter_map(|state| match state {
                InheritedDefaultState::Unique(defaults, provider) => {
                    Some((defaults.clone(), *provider))
                }
                InheritedDefaultState::Absent | InheritedDefaultState::Ambiguous => None,
            });
            let first = unique.next();
            let state = if candidates.contains(&InheritedDefaultState::Ambiguous) {
                InheritedDefaultState::Ambiguous
            } else if let Some(first) = first {
                if unique.all(|candidate| candidate == first) {
                    InheritedDefaultState::Unique(first.0, first.1)
                } else {
                    InheritedDefaultState::Ambiguous
                }
            } else {
                InheritedDefaultState::Absent
            };
            additions.push((implementation, state));
        }
        if additions.is_empty() {
            break;
        }
        for (implementation, state) in additions {
            states.insert(implementation, state);
        }
    }
    let mut inherited_defaults = states.into_iter().collect::<Vec<_>>();
    inherited_defaults.sort_by_key(|(callable, _)| callable.raw());
    for (callable, state) in inherited_defaults {
        let InheritedDefaultState::Unique(defaults, provider) = state else {
            continue;
        };
        if provider != ResolvedFunctionOverrideTarget::Module(callable) {
            index.publish_inherited_callable_defaults(callable, &defaults, provider);
        }
    }
}

pub(crate) fn publish_override_plans(index: &mut ResolvedModuleIndex, table: &SymbolTable) {
    let plans = {
        let module = ModuleSymbols::new(table);
        let source =
            CompositeSource::new(vec![&module as &dyn SymbolSource, table.libraries.as_ref()]);
        table
            .classes
            .values()
            .filter_map(|class| {
                class.stable_declaration.and_then(|declaration| {
                    // A body-local classifier whose semantic header is deferred to Pass 2 cannot
                    // own a Pass-1 override plan. Its checked lexical publication must supply the
                    // hierarchy and override identities together.
                    index
                        .classifier_header(declaration)
                        .filter(|_| {
                            !index.declaration_header(declaration).is_some_and(|header| {
                                header.flags.has(DeclarationFlags::LOCAL_CLASS)
                            })
                        })
                        .map(|_| (declaration, class))
                })
            })
            .map(|(classifier, class)| {
                let hierarchy = index
                    .classifier_hierarchy(classifier)
                    .unwrap_or_default()
                    .to_vec();
                (
                    classifier,
                    property_override_plans(index, &source, class, &hierarchy),
                    function_override_plans(index, &source, class, &hierarchy),
                )
            })
            .chain(enum_entry_override_plans(index, &source))
            .collect::<Vec<_>>()
    };
    let function_edges = plans
        .iter()
        .flat_map(|(_, _, functions)| functions.iter().cloned())
        .collect::<Vec<_>>();
    publish_inherited_function_defaults(index, &function_edges);
    for (classifier, properties, functions) in plans {
        index.publish_property_overrides(classifier, properties);
        index.publish_function_overrides(classifier, functions);
    }
}
