//! Publication of exact Kotlin override edges.
//!
//! Providers and the module declaration model are live only in Pass 1. This module performs the
//! declaration pairing there and stores a compact, provider-neutral edge in stable FIR. It never
//! computes a target descriptor or decides whether a particular backend needs a bridge.

use std::collections::{HashMap, HashSet};

mod inherited_status;
mod published_recheck;

pub(super) use published_recheck::published_function_override_matches;

use super::SymbolTable;
use crate::fir::{
    DeclarationFlags, DeclarationId, DeclarationKind, ResolvedAppliedClassifier,
    ResolvedFunctionOverride, ResolvedFunctionOverrideTarget, ResolvedModuleIndex,
    ResolvedPropertyOverride, ResolvedPropertyOverrideTarget, ResolvedTy,
};
use crate::libraries::{FnKind, FunctionInfo, PropKind, PropertyInfo};
use crate::module_symbols::ModuleSymbols;
use crate::symbol_source::{CompositeSource, SymbolSource};
use crate::types::{Ty, TypeName, Visibility};

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

/// The unsubstituted declared shape of the exact declaration a property selects: the signature a
/// generated implementation (an interface delegation forwarder) must bridge to.
pub(super) struct DeclaredProperty {
    pub(super) target: ResolvedPropertyOverrideTarget,
    pub(super) ty: Ty,
    /// A member extension's declared receiver.
    pub(super) receiver: Option<Ty>,
}

pub(super) fn property_declaration(
    index: &ResolvedModuleIndex,
    property: &PropertyInfo,
) -> Option<DeclaredProperty> {
    let target = target(index, property)?;
    let (ty, receiver) = match target {
        ResolvedPropertyOverrideTarget::Module(id) => (
            index.signature(property.stable_declaration?)?.result.get(),
            index.property(id)?.extension_receiver.map(ResolvedTy::get),
        ),
        ResolvedPropertyOverrideTarget::External(_) => {
            let getter = &property.getter;
            let receiver = match property.kind {
                PropKind::MemberExtension => Some(
                    *getter
                        .declared_params
                        .as_deref()
                        .or_else(|| getter.generic_sig.as_ref().map(|sig| sig.params.as_slice()))
                        .unwrap_or(&getter.params)
                        .get(property.context_count)?,
                ),
                PropKind::Member | PropKind::Extension | PropKind::TopLevel => None,
            };
            let ty = getter
                .declared_ret
                .or_else(|| getter.generic_sig.as_ref().map(|signature| signature.ret))
                .unwrap_or(getter.ret);
            (ty, receiver)
        }
    };
    Some(DeclaredProperty {
        target,
        ty,
        receiver,
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

/// `owner`'s own non-private properties named `name` of one kind: member properties, or (with
/// `member_extensions`) member-extension properties, whose receiver is part of their override slot.
fn declared_properties(
    source: &dyn crate::symbol_source::SymbolSource,
    owner: Ty,
    name: &str,
    member_extensions: bool,
) -> Vec<PropertyInfo> {
    let kind = if member_extensions {
        PropKind::MemberExtension
    } else {
        PropKind::Member
    };
    crate::symbol_resolver::declared_member_callables(source, owner, name)
        .into_parts()
        .1
        .overloads
        .into_iter()
        .filter(|property| {
            property.kind == kind
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

/// A dependency declaration's own parameters (its extension receiver in place) and result, before
/// any substitution, as the provider that assigned `identity` normalized it: the declaration's
/// value-class spelling, else its generic signature, else the shape the provider published. A
/// declaration found through an applied supertype (`Echo<String>.echo: String`) still answers with
/// its own (`Echo<T>.echo: T`).
pub(super) fn dependency_declaration_signature(
    source: &dyn SymbolSource,
    identity: crate::fir::ExternalCallableId,
) -> (Box<[Ty]>, Ty) {
    let realization = source
        .external_callable(identity)
        .expect("the provider that assigned a dependency identity answers for it");
    let callable = &realization.callable;
    let parameters = callable
        .declared_params
        .clone()
        .or_else(|| {
            callable.generic_sig.as_ref().map(|signature| {
                if realization.kind == crate::libraries::ExternalCallableKind::Extension {
                    signature.parameters_with_receiver(callable.context_count)
                } else {
                    signature.params.clone().into_boxed_slice()
                }
            })
        })
        .unwrap_or_else(|| callable.params.clone().into_boxed_slice());
    let result = callable
        .declared_ret
        .or_else(|| callable.generic_sig.as_ref().map(|signature| signature.ret))
        .unwrap_or(callable.ret);
    (parameters, result)
}

/// The overridden declaration's own parameters and result, before any substitution. A backend
/// erases these for the supertype side of a bridge without reopening the declaration's provider.
fn overridden_declaration_signature(
    source: &dyn SymbolSource,
    declared: &FunctionInfo,
    overridden: ResolvedFunctionOverrideTarget,
) -> (Box<[ResolvedTy]>, ResolvedTy) {
    let (parameters, result) = match overridden {
        ResolvedFunctionOverrideTarget::Module(_) => (
            declaration_parameters_with_receiver(declared).into_boxed_slice(),
            declared.ret.apply(declared.callable.ret),
        ),
        ResolvedFunctionOverrideTarget::External(identity) => {
            dependency_declaration_signature(source, identity)
        }
    };
    (
        resolved_types(
            parameters.iter().copied(),
            "overridden function declaration parameters",
        ),
        resolved_ty(result, "overridden function result"),
    )
}

fn applied_parameters_with_receiver(function: &crate::libraries::FunctionInfo) -> Vec<Ty> {
    declaration_parameters_with_receiver(function)
}

/// The overridden declaration's parameter identities: a module callable publishes them in the
/// index, as an implementation's do; a dependency declaration carries them normalized.
fn overridden_parameter_identities(
    index: &ResolvedModuleIndex,
    overridden: ResolvedFunctionOverrideTarget,
    declared: &crate::libraries::FunctionInfo,
    count: usize,
) -> Box<[crate::fir::ResolvedParameterIdentity]> {
    match overridden {
        ResolvedFunctionOverrideTarget::Module(callable) => {
            module_parameter_identities(index, callable, count)
        }
        ResolvedFunctionOverrideTarget::External(_) => {
            function_parameter_identities(declared, count)
        }
    }
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

/// Declared upper bounds of a declaration's own type parameters, parallel to
/// [`declaration_formals`]; an empty entry is the implicit `Any?`.
fn declaration_formal_bounds(
    index: &ResolvedModuleIndex,
    declaration: crate::fir::DeclarationId,
) -> Vec<Vec<Ty>> {
    (0..)
        .map_while(|ordinal| index.type_parameter(declaration, ordinal))
        .filter_map(|parameter| {
            index.type_parameter_semantic_name(parameter)?;
            index.type_parameter_header(parameter)
        })
        .map(|header| header.bounds.iter().map(|bound| bound.ty.get()).collect())
        .collect()
}

/// Exact interface classifiers inherited through one source classifier's direct superclass.
///
/// This is path provenance, not another member lookup: the full applied hierarchy is already a
/// stable FIR fact, but flattening it loses whether an interface arrived through the superclass or
/// through a directly declared interface. Compatibility emitters need that distinction without
/// reopening providers after checking.
fn inherited_superclass_interfaces(
    index: &ResolvedModuleIndex,
    source: &dyn SymbolSource,
    classifier: crate::fir::DeclarationId,
) -> Vec<TypeName> {
    let Some(superclass) = index
        .classifier_header(classifier)
        .and_then(|header| header.superclass)
    else {
        return Vec::new();
    };
    let mut seen = HashSet::new();
    crate::symbol_resolver::applied_hierarchy(source, superclass.get())
        .into_iter()
        .filter_map(|(candidate, _, _)| {
            (source
                .classifier(candidate)
                .is_some_and(|shape| shape.is_interface())
                && seen.insert(candidate))
            .then_some(candidate)
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
                let (declared_parameters, declared_result) =
                    overridden_declaration_signature(source, declared, overridden);
                let declared_parameter_count = declared_parameters.len();
                let implementation_parameters =
                    declaration_parameters_with_receiver(implementation_declared);
                overrides.push(ResolvedFunctionOverride {
                    implementation: implementation_target,
                    implementation_owner: implementation.callable.owner,
                    overridden,
                    overridden_owner: supertype.classifier,
                    overridden_semantic_role: declared.callable.semantic_role,
                    collection_barrier: declared.callable.collection_barrier,
                    overridden_is_interface: true,
                    name: name.clone().into_boxed_str(),
                    declared_parameters,
                    declared_result,
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
                    overridden_parameter_identities: overridden_parameter_identities(
                        index,
                        overridden,
                        declared,
                        declared_parameter_count,
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
                    overridden_return_value_status: applied.flags.return_value_status,
                    overridden_operator: applied.flags.operator,
                    overridden_infix: applied.flags.infix,
                    overridden_visibility: applied.visibility,
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
                declared_properties(source, Ty::obj_name(supertype.classifier), name, false),
                |property| target(index, property),
            );
            for applied in declared_properties(source, supertype.applied.get(), name, false) {
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
                    declared_properties(source, Ty::obj_name(implementation.owner), name, false),
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
                    declared_receiver: None,
                    implementation_receiver: None,
                    implementation_mutable: implementation.setter.is_some(),
                    overridden_return_value_status: applied.return_value_status,
                    overridden_visibility: applied.visibility,
                    has_kotlin_superclass_override: false,
                    depth: supertype.depth,
                });
            }
        }
    }
}

/// A source property declaration that overrides, as the override edges see it.
struct PropertyImplementation<'a> {
    owner: crate::types::TypeName,
    name: &'a str,
    id: crate::fir::PropertyId,
    ty: ResolvedTy,
    formals: Vec<String>,
    formal_bounds: Vec<Vec<Ty>>,
    context_parameters: Vec<Ty>,
    /// The declared receiver of a member-extension property, part of the slot it overrides.
    receiver: Option<ResolvedTy>,
    mutable: bool,
}

fn property_implementation<'a>(
    index: &ResolvedModuleIndex,
    owner: crate::types::TypeName,
    name: &'a str,
    declaration: crate::fir::DeclarationId,
) -> Option<PropertyImplementation<'a>> {
    let signature = index.signature(declaration)?;
    let property = index.property(index.property_for_declaration(declaration)?)?;
    let context_count = usize::try_from(property.context_parameter_count).ok()?;
    Some(PropertyImplementation {
        owner,
        name,
        id: property.id,
        ty: signature.result,
        formals: declaration_formals(index, declaration),
        formal_bounds: declaration_formal_bounds(index, declaration),
        context_parameters: signature
            .parameters
            .get(..context_count)?
            .iter()
            .map(|parameter| parameter.get())
            .collect(),
        receiver: property.extension_receiver,
        mutable: property.mutable,
    })
}

fn append_property_override_edges(
    index: &ResolvedModuleIndex,
    source: &dyn SymbolSource,
    implementation: &PropertyImplementation<'_>,
    hierarchy: &[crate::fir::ResolvedAppliedClassifier],
    seen: &mut HashSet<ResolvedPropertyOverrideTarget>,
    overrides: &mut Vec<ResolvedPropertyOverride>,
) {
    let implementation_owner = implementation.owner;
    let name = implementation.name;
    let implementation_id = implementation.id;
    let implementation_type = implementation.ty;
    let implementation_receiver = implementation.receiver;
    let implementation_mutable = implementation.mutable;
    let member_extension = implementation_receiver.is_some();
    let first = overrides.len();
    for supertype in hierarchy.iter().filter(|entry| entry.depth != 0) {
        let overridden_is_interface = source
            .classifier(supertype.classifier)
            .is_some_and(|classifier| classifier.is_interface());
        let raw = declarations_by_target(
            declared_properties(
                source,
                Ty::obj_name(supertype.classifier),
                name,
                member_extension,
            ),
            |property| target(index, property),
        );
        for applied in declared_properties(source, supertype.applied.get(), name, member_extension)
        {
            let Some(overridden) = target(index, &applied) else {
                continue;
            };
            let Some(declared) = raw.get(&overridden) else {
                continue;
            };
            let applied_formals = applied.formals.as_slice();
            let applied_bounds = applied
                .getter
                .generic_sig
                .as_ref()
                .map(|signature| signature.formal_bounds.as_slice())
                .unwrap_or_default();
            let applied_context_parameters = applied
                .getter
                .params
                .get(..applied.context_count)
                .unwrap_or_default();
            let compatible_inputs = crate::symbol_resolver::override_input_shapes_match(
                source,
                crate::symbol_resolver::OverrideInputShape {
                    params: applied_context_parameters,
                    receiver: applied.receiver.filter(|_| member_extension),
                    formals: applied_formals,
                    formal_bounds: applied_bounds,
                    context_count: applied.context_count,
                    suspend: false,
                },
                crate::symbol_resolver::OverrideInputShape {
                    params: &implementation.context_parameters,
                    receiver: implementation_receiver.map(ResolvedTy::get),
                    formals: &implementation.formals,
                    formal_bounds: &implementation.formal_bounds,
                    context_count: implementation.context_parameters.len(),
                    suspend: false,
                },
            );
            let compatible_type = if applied.setter.is_some() {
                crate::symbol_resolver::override_parameter_types_match(
                    source,
                    &[applied.ty],
                    applied_formals,
                    &[implementation_type.get()],
                    &implementation.formals,
                )
            } else {
                crate::symbol_resolver::resolution_subtype(
                    source,
                    crate::types::ty_canonicalize_params(
                        implementation_type.get().canonical_semantic(),
                        &implementation.formals,
                    ),
                    crate::types::ty_canonicalize_params(
                        applied.ty.canonical_semantic(),
                        applied_formals,
                    ),
                )
            };
            if !compatible_inputs
                || !compatible_type
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
                declared_receiver: declared
                    .receiver
                    .filter(|_| member_extension)
                    .map(|receiver| {
                        resolved_ty(
                            receiver,
                            "provider declaration receiver entering stable FIR",
                        )
                    }),
                implementation_receiver,
                overridden_mutable: applied.setter.is_some(),
                implementation_mutable,
                overridden_return_value_status: applied.return_value_status,
                overridden_visibility: applied.visibility,
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

/// `declaration` as an override edge's implementation, when it is a stable `override` property.
fn overriding_property<'a>(
    index: &ResolvedModuleIndex,
    owner: crate::types::TypeName,
    name: &'a str,
    declaration: crate::fir::DeclarationId,
) -> Option<PropertyImplementation<'a>> {
    if !index
        .declaration_header(declaration)?
        .flags
        .has(DeclarationFlags::OVERRIDE)
    {
        return None;
    }
    let mut implementation = property_implementation(index, owner, name, declaration)?;
    implementation.ty = ResolvedTy::new(implementation.ty.get().canonical_semantic()).ok()?;
    implementation.receiver = implementation
        .receiver
        .map(|receiver| ResolvedTy::new(receiver.get().canonical_semantic()))
        .transpose()
        .ok()?;
    Some(implementation)
}

fn property_override_plans(
    index: &ResolvedModuleIndex,
    source: &dyn SymbolSource,
    class: &super::ClassSig,
    hierarchy: &[crate::fir::ResolvedAppliedClassifier],
) -> Vec<ResolvedPropertyOverride> {
    let mut overrides = Vec::new();
    let mut seen = HashSet::new();
    let names = class
        .declared_props
        .keys()
        .chain(class.member_ext_props.keys());
    for name in declaration_ordered(class, names) {
        let members = class
            .declared_props
            .get(name)
            .map(|property| property.stable_declaration);
        let member_extensions = class
            .member_ext_props
            .get(name)
            .into_iter()
            .flatten()
            .map(|property| property.stable_declaration);
        for declaration in members.into_iter().chain(member_extensions).flatten() {
            let Some(implementation) =
                overriding_property(index, class.internal_name(), name, declaration)
            else {
                continue;
            };
            append_property_override_edges(
                index,
                source,
                &implementation,
                hierarchy,
                &mut seen,
                &mut overrides,
            );
        }
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

/// The overriding side of an override edge: one module declaration's own formals, input shape
/// (context parameters, then the extension receiver, then value parameters) and result.
struct OverridingFunction {
    callable: crate::fir::CallableId,
    formals: Vec<String>,
    formal_bounds: Vec<Vec<Ty>>,
    parameters: Vec<Ty>,
    receiver: Option<Ty>,
    context_count: usize,
    result: Ty,
    suspend: bool,
}

impl OverridingFunction {
    fn of(
        index: &ResolvedModuleIndex,
        declaration: crate::fir::DeclarationId,
        callable: &crate::fir::ResolvedCallableHeader,
        signature: &crate::fir::ResolvedSignature,
        suspend: bool,
    ) -> Self {
        let mut parameters = signature
            .parameters
            .iter()
            .map(|parameter| parameter.get())
            .collect::<Vec<_>>();
        let context_count = callable.shape.context_parameter_count as usize;
        if let Some(receiver) = callable.shape.extension_receiver {
            parameters.insert(context_count.min(parameters.len()), receiver.get());
        }
        Self {
            callable: callable.id,
            formals: declaration_formals(index, declaration),
            formal_bounds: declaration_formal_bounds(index, declaration),
            parameters,
            receiver: callable
                .shape
                .extension_receiver
                .map(crate::fir::ResolvedTy::get),
            context_count,
            result: signature.result.get().canonical_semantic(),
            suspend,
        }
    }
}

fn append_function_override_edges(
    index: &ResolvedModuleIndex,
    source: &dyn SymbolSource,
    implementation_owner: crate::types::TypeName,
    name: &str,
    implementation: &OverridingFunction,
    hierarchy: &[crate::fir::ResolvedAppliedClassifier],
    overrides: &mut Vec<ResolvedFunctionOverride>,
) {
    let mut seen = HashSet::new();
    let first = overrides.len();
    let mut implementation_inputs = implementation.parameters.to_vec();
    if implementation.receiver.is_some() {
        assert!(
            implementation.context_count < implementation_inputs.len(),
            "an extension override must retain its declared receiver parameter"
        );
        implementation_inputs.remove(implementation.context_count);
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
            let applied_bounds = applied
                .generic_sig
                .as_ref()
                .map(|signature| signature.formal_bounds.as_slice())
                .unwrap_or_default();
            if !crate::symbol_resolver::override_input_shapes_match(
                source,
                crate::symbol_resolver::OverrideInputShape {
                    params: &applied_inputs,
                    receiver: applied
                        .semantic_receiver()
                        .filter(|_| applied.is_extension()),
                    formals: applied_formals,
                    formal_bounds: applied_bounds,
                    context_count: applied.context_count,
                    suspend: applied.flags.suspend,
                },
                crate::symbol_resolver::OverrideInputShape {
                    params: &implementation_inputs,
                    receiver: implementation.receiver,
                    formals: &implementation.formals,
                    formal_bounds: &implementation.formal_bounds,
                    context_count: implementation.context_count,
                    suspend: implementation.suspend,
                },
            ) || !crate::symbol_resolver::resolution_subtype(
                source,
                crate::types::ty_canonicalize_params(
                    implementation.result,
                    &implementation.formals,
                ),
                crate::types::ty_canonicalize_params(
                    applied.ret.apply(applied.callable.ret).canonical_semantic(),
                    applied_formals,
                ),
            ) || !seen.insert(overridden)
            {
                continue;
            }
            let (declared_parameters, declared_result) =
                overridden_declaration_signature(source, declared, overridden);
            let declared_parameter_count = declared_parameters.len();
            overrides.push(ResolvedFunctionOverride {
                implementation: ResolvedFunctionOverrideTarget::Module(implementation.callable),
                implementation_owner,
                overridden,
                overridden_owner: supertype.classifier,
                overridden_semantic_role: declared.callable.semantic_role,
                collection_barrier: declared.callable.collection_barrier,
                overridden_is_interface,
                name: name.into(),
                declared_parameters,
                declared_result,
                applied_parameters: resolved_types(
                    applied_parameters.iter().copied(),
                    "applied overridden function parameters",
                ),
                applied_result: resolved_ty(
                    applied.ret.apply(applied.callable.ret),
                    "applied overridden function result",
                ),
                implementation_parameters: resolved_types(
                    implementation.parameters.iter().copied(),
                    "overriding function parameters",
                ),
                implementation_parameter_identities: module_parameter_identities(
                    index,
                    implementation.callable,
                    implementation.parameters.len(),
                ),
                overridden_parameter_identities: overridden_parameter_identities(
                    index,
                    overridden,
                    declared,
                    declared_parameter_count,
                ),
                implementation_result: resolved_ty(
                    implementation.result,
                    "overriding function result",
                ),
                overridden_parameter_defaults: function_default_bitmap(&applied),
                overridden_default_provider: function_default_provider(index, &applied, overridden),
                overridden_return_value_status: applied.flags.return_value_status,
                overridden_operator: applied.flags.operator,
                overridden_infix: applied.flags.infix,
                overridden_visibility: applied.visibility,
                suspend: implementation.suspend,
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

/// The hierarchy an `invoke` override is matched against. A function-type supertype
/// (`class C : suspend (A) -> R`) stays a callable shape in the published hierarchy; its `invoke`
/// is declared by the function classifier the shape is an instance of, so that classifier joins the
/// walk one rung below the classifier that declares the supertype.
fn with_function_supertypes(
    source: &dyn SymbolSource,
    hierarchy: &[crate::fir::ResolvedAppliedClassifier],
) -> Vec<crate::fir::ResolvedAppliedClassifier> {
    let mut extended = hierarchy.to_vec();
    for entry in hierarchy {
        let Some(declaring) = source.classifier(entry.classifier) else {
            continue;
        };
        let bindings = crate::symbol_resolver::classifier_bindings(&declaring, entry.applied.get());
        for signature in &declaring.callable_signatures {
            let applied = crate::types::ty_subst_applied_arguments(*signature, &bindings);
            let view = crate::libraries::function_classifiers::supertype_classifier(applied);
            let Some(classifier) = view.obj_internal() else {
                continue;
            };
            if extended.iter().any(|known| known.classifier == classifier) {
                continue;
            }
            extended.push(crate::fir::ResolvedAppliedClassifier {
                classifier,
                applied: ResolvedTy::new(view).expect("a finalized function supertype is resolved"),
                depth: entry.depth + 1,
            });
        }
    }
    extended
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
        let implementation = OverridingFunction::of(
            index,
            declaration,
            &implementation_callable,
            implementation_signature,
            implementation.is_suspend(),
        );
        append_function_override_edges(
            index,
            source,
            class.internal_name(),
            name,
            &implementation,
            hierarchy,
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
                    let Some(implementation) =
                        property_implementation(index, implementation_owner, name, member)
                    else {
                        continue;
                    };
                    append_property_override_edges(
                        index,
                        source,
                        &implementation,
                        &hierarchy,
                        &mut property_seen,
                        &mut properties,
                    );
                }
                DeclarationKind::Function => {
                    let Some(callable) = index.callable_for_declaration(member) else {
                        continue;
                    };
                    let implementation = OverridingFunction::of(
                        index,
                        member,
                        &callable,
                        signature,
                        header.flags.has(DeclarationFlags::SUSPEND),
                    );
                    append_function_override_edges(
                        index,
                        source,
                        implementation_owner,
                        name,
                        &implementation,
                        &hierarchy,
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
    let (plans, preorders, superclass_interfaces) = {
        let module = crate::fir::StreamedModuleSymbols::for_file(index, source_file);
        let source = CompositeSource::new(vec![
            &module as &dyn SymbolSource,
            platform as &dyn SymbolSource,
        ]);
        let plans = {
            classifiers
                .iter()
                .copied()
                .filter_map(|classifier| {
                    let header = index.declaration_header(classifier)?;
                    if !header.flags.has(DeclarationFlags::LOCAL_CLASS) {
                        return None;
                    }
                    let implementation_owner = index.classifier_header(classifier)?.classifier;
                    let hierarchy =
                        with_function_supertypes(&source, index.classifier_hierarchy(classifier)?);
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
                                let Some(implementation) = property_implementation(
                                    index,
                                    implementation_owner,
                                    name,
                                    declaration,
                                ) else {
                                    continue;
                                };
                                append_property_override_edges(
                                    index,
                                    &source,
                                    &implementation,
                                    &hierarchy,
                                    &mut property_seen,
                                    &mut properties,
                                );
                            }
                            crate::fir::DeclarationKind::Function => {
                                let Some(callable) = index.callable_for_declaration(declaration)
                                else {
                                    continue;
                                };
                                let implementation = OverridingFunction::of(
                                    index,
                                    declaration,
                                    &callable,
                                    signature,
                                    member.flags.has(DeclarationFlags::SUSPEND),
                                );
                                append_function_override_edges(
                                    index,
                                    &source,
                                    implementation_owner,
                                    name,
                                    &implementation,
                                    &hierarchy,
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
        let preorders = default_supertype_orders(&source, &function_edges);
        let superclass_interfaces = plans
            .iter()
            .map(|(classifier, _, _)| {
                (
                    *classifier,
                    inherited_superclass_interfaces(index, &source, *classifier),
                )
            })
            .collect::<Vec<_>>();
        (plans, preorders, superclass_interfaces)
    };
    let function_edges = plans
        .iter()
        .flat_map(|(_, _, functions)| functions.iter().cloned())
        .collect::<Vec<_>>();
    publish_inherited_function_defaults(index, &preorders, &function_edges);
    for (classifier, interfaces) in superclass_interfaces {
        if index.superclass_interfaces(classifier).is_none() {
            index.publish_superclass_interfaces(classifier, interfaces);
        }
    }
    // A local classifier's plan is published once, by the first body that checks it (a default
    // argument's object is checked for each function that carries the default); its statuses go
    // with that first publication.
    crate::trace_compiler!(
        "override",
        "local override plans classifiers={:?} plans={:?}",
        classifiers,
        plans
            .iter()
            .map(|(classifier, properties, functions)| (
                classifier,
                properties.len(),
                functions.len(),
                index.has_function_override_plan(*classifier),
            ))
            .collect::<Vec<_>>(),
    );
    let unpublished = plans
        .iter()
        .map(|(classifier, properties, functions)| {
            (
                *classifier,
                if index.has_property_override_plan(*classifier) {
                    Vec::new()
                } else {
                    properties.clone()
                },
                if index.has_function_override_plan(*classifier) {
                    Vec::new()
                } else {
                    functions.clone()
                },
            )
        })
        .collect::<Vec<_>>();
    // A local classifier's members are projected from the FIR headers the visibility publication
    // just corrected; there is no module symbol table copy to follow.
    let _ = publish_inherited_statuses(index, &unpublished);

    for (classifier, properties, functions) in plans {
        if !index.has_property_override_plan(classifier) {
            index.publish_property_overrides(classifier, properties);
        }
        if !index.has_function_override_plan(classifier) {
            index.publish_function_overrides(classifier, functions);
        }
    }
}

fn publish_inherited_statuses(
    index: &mut ResolvedModuleIndex,
    plans: &[(
        crate::fir::DeclarationId,
        Vec<ResolvedPropertyOverride>,
        Vec<ResolvedFunctionOverride>,
    )],
) -> Vec<(crate::fir::DeclarationId, Visibility)> {
    let classifiers = plans
        .iter()
        .map(
            |(classifier, properties, functions)| inherited_status::ClassifierEdges {
                classifier: *classifier,
                functions,
                properties,
            },
        )
        .collect::<Vec<_>>();
    inherited_status::publish_inherited_statuses(index, &classifiers)
}

/// Record an override's inherited visibility on the module's projected member signatures, so Pass
/// 2 access checks and call routing see the same visibility the FIR header (and with it every
/// backend) now records. Streamed local members have no projected signatures; their providers read
/// the corrected header directly.
fn publish_inherited_visibilities_to_symbols(
    table: &mut SymbolTable,
    inherited: &[(crate::fir::DeclarationId, Visibility)],
) {
    if inherited.is_empty() {
        return;
    }
    let inherited: HashMap<crate::fir::DeclarationId, Visibility> =
        inherited.iter().copied().collect();
    table.begin_module_mutation();
    for class in table.classes.values_mut() {
        for overloads in class.methods.values_mut() {
            for signature in overloads.iter_mut() {
                if let Some(visibility) = signature
                    .stable_declaration
                    .and_then(|declaration| inherited.get(&declaration))
                {
                    signature.visibility = *visibility;
                }
            }
        }
        for overloads in class.member_ext_funs.values_mut() {
            for member in overloads.iter_mut() {
                if let Some(visibility) = member
                    .signature
                    .stable_declaration
                    .and_then(|declaration| inherited.get(&declaration))
                {
                    member.signature.visibility = *visibility;
                }
            }
        }
        for property in class.declared_props.values_mut() {
            if let Some(visibility) = property
                .stable_declaration
                .and_then(|declaration| inherited.get(&declaration))
            {
                property.visibility = *visibility;
            }
        }
        for overloads in class.contextual_props.values_mut() {
            for property in overloads.iter_mut() {
                if let Some(visibility) = property
                    .stable_declaration
                    .and_then(|declaration| inherited.get(&declaration))
                {
                    property.visibility = *visibility;
                }
            }
        }
        for overloads in class.member_ext_props.values_mut() {
            for property in overloads.iter_mut() {
                if let Some(visibility) = property
                    .stable_declaration
                    .and_then(|declaration| inherited.get(&declaration))
                {
                    property.visibility = *visibility;
                }
            }
        }
    }
    table.finish_module_mutation();
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

/// Cache each implementation owner's typed left-to-right supertype order while the composite
/// symbol source that owns the hierarchy is live.
fn default_supertype_orders(
    source: &dyn SymbolSource,
    edges: &[ResolvedFunctionOverride],
) -> HashMap<TypeName, HashMap<TypeName, u32>> {
    let mut orders = HashMap::new();
    for edge in edges {
        orders.entry(edge.implementation_owner).or_insert_with(|| {
            crate::symbol_resolver::supertype_preorder(source, edge.implementation_owner)
        });
    }
    orders
}

/// Choose one inherited default provider, or report that the clash is ambiguous.
///
/// Immediate supertypes conflict only when distinct providers both default the same parameter
/// slot. Disjoint defaults contribute one combined omission bitmap, but the expression provider is
/// still the declaration found first in a left-to-right depth-first walk. A default visible only
/// through an intermediate classifier uses that same order even when a later supertype declares a
/// nearer default (`KT-36188`).
fn inherited_default_from_suppliers(
    preorders: &HashMap<TypeName, HashMap<TypeName, u32>>,
    edges: &[&ResolvedFunctionOverride],
    saw_ambiguous: bool,
    suppliers: Vec<(u32, TypeName, Vec<bool>, ResolvedFunctionOverrideTarget)>,
) -> InheritedDefaultState {
    if saw_ambiguous || suppliers.is_empty() {
        return if saw_ambiguous {
            InheritedDefaultState::Ambiguous
        } else {
            InheritedDefaultState::Absent
        };
    }
    let immediate = suppliers
        .iter()
        .filter(|(depth, _, _, _)| *depth == 1)
        .collect::<Vec<_>>();
    for (index, (_, _, defaults, provider)) in immediate.iter().enumerate() {
        for (_, _, other_defaults, other_provider) in &immediate[index + 1..] {
            if provider != other_provider
                && defaults.iter().enumerate().any(|(parameter, left)| {
                    *left && other_defaults.get(parameter).copied().unwrap_or(false)
                })
            {
                return InheritedDefaultState::Ambiguous;
            }
        }
    }
    let order = edges
        .first()
        .and_then(|edge| preorders.get(&edge.implementation_owner));
    let (_, _, _, provider) = suppliers
        .iter()
        .min_by_key(|(depth, owner, _, _)| {
            (
                order
                    .and_then(|order| order.get(owner).copied())
                    .unwrap_or(u32::MAX),
                *depth,
            )
        })
        .expect("at least one inherited default");
    let provider = *provider;
    let mut defaults = Vec::new();
    for (_, _, supplier_defaults, _) in suppliers {
        defaults.resize(defaults.len().max(supplier_defaults.len()), false);
        for (target, supplied) in defaults.iter_mut().zip(supplier_defaults) {
            *target |= supplied;
        }
    }
    InheritedDefaultState::Unique(defaults, provider)
}

fn publish_inherited_function_defaults(
    index: &mut ResolvedModuleIndex,
    preorders: &HashMap<TypeName, HashMap<TypeName, u32>>,
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
            let mut suppliers = Vec::new();
            let mut saw_ambiguous = false;
            let mut unresolved = false;
            for edge in &edges {
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
                match state {
                    InheritedDefaultState::Unique(defaults, provider) => {
                        suppliers.push((edge.depth, edge.overridden_owner, defaults, provider));
                    }
                    InheritedDefaultState::Ambiguous => saw_ambiguous = true,
                    InheritedDefaultState::Absent => {}
                }
            }
            if unresolved {
                continue;
            }
            let state =
                inherited_default_from_suppliers(preorders, &edges, saw_ambiguous, suppliers);
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

pub(crate) fn publish_override_plans(index: &mut ResolvedModuleIndex, table: &mut SymbolTable) {
    let (plans, preorders, superclass_interfaces) = {
        let module = ModuleSymbols::new(table);
        let source =
            CompositeSource::new(vec![&module as &dyn SymbolSource, table.libraries.as_ref()]);
        let plans = {
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
                    let hierarchy = with_function_supertypes(
                        &source,
                        index.classifier_hierarchy(classifier).unwrap_or_default(),
                    );
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
        let preorders = default_supertype_orders(&source, &function_edges);
        let superclass_interfaces = plans
            .iter()
            .filter_map(|(classifier, _, _)| {
                index.classifier_header(*classifier).map(|_| {
                    (
                        *classifier,
                        inherited_superclass_interfaces(index, &source, *classifier),
                    )
                })
            })
            .collect::<Vec<_>>();
        (plans, preorders, superclass_interfaces)
    };
    let function_edges = plans
        .iter()
        .flat_map(|(_, _, functions)| functions.iter().cloned())
        .collect::<Vec<_>>();
    publish_inherited_function_defaults(index, &preorders, &function_edges);
    for (classifier, interfaces) in superclass_interfaces {
        index.publish_superclass_interfaces(classifier, interfaces);
    }
    let inherited = publish_inherited_statuses(index, &plans);
    publish_inherited_visibilities_to_symbols(table, &inherited);
    for (classifier, properties, functions) in plans {
        index.publish_property_overrides(classifier, properties);
        index.publish_function_overrides(classifier, functions);
    }
}
