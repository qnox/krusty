//! Pass-1 resolution of interface-delegation forwarding declarations.
//!
//! Providers expose direct declarations and direct supertypes. This module applies the delegated
//! interface, closes that hierarchy through the common symbol source, selects one effective
//! declaration per Kotlin override slot, and publishes only pending-free forwarding facts. Common
//! lowering must never repeat this walk.

use std::collections::{HashSet, VecDeque};

use crate::fir::{
    ResolvedDelegatedCall, ResolvedDelegatedCallTarget, ResolvedDelegatedContextParameter,
    ResolvedDelegatedFunction, ResolvedDelegatedFunctionDeclaration, ResolvedDelegatedMember,
    ResolvedDelegatedModuleTarget, ResolvedDelegatedProperty, ResolvedDelegatedTypeParameter,
    ResolvedFunctionOverrideTarget, ResolvedInterfaceDelegateSource, ResolvedInterfaceDelegation,
    ResolvedModuleIndex, ResolvedTy,
};
use crate::libraries::{FnKind, FunctionInfo, PropKind, PropertyInfo};
use crate::symbol_source::{CompositeSource, SymbolSource};
use crate::types::{Ty, TypeName};

use super::SymbolTable;

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct FunctionSlot {
    name: String,
    context_count: usize,
    extension: bool,
    suspend: bool,
    parameters: Vec<Ty>,
}

fn applied_function_parameters(function: &FunctionInfo) -> Vec<Ty> {
    let mut parameters = function.semantic_params().to_vec();
    if function.kind == FnKind::Extension {
        let receiver = function
            .semantic_receiver()
            .expect("a normalized extension declaration has a receiver");
        parameters.insert(function.context_count.min(parameters.len()), receiver);
    }
    parameters
}

fn function_slot(function: &FunctionInfo) -> FunctionSlot {
    let formals = function
        .generic_sig
        .as_ref()
        .map(|signature| signature.formals.as_slice())
        .unwrap_or_default();
    FunctionSlot {
        name: function.callable.name.clone(),
        context_count: function.context_count,
        extension: function.kind == FnKind::Extension,
        suspend: function.flags.suspend,
        parameters: applied_function_parameters(function)
            .into_iter()
            .map(|parameter| crate::types::ty_canonicalize_params(parameter, formals))
            .collect(),
    }
}

fn hierarchy_names(source: &dyn SymbolSource, root: Ty) -> Option<Vec<String>> {
    let mut queue = VecDeque::from([root]);
    let mut seen = HashSet::new();
    let mut names = Vec::new();
    while let Some(current) = queue.pop_front() {
        let classifier = current.kotlin_class_internal()?;
        if !seen.insert(classifier) {
            continue;
        }
        let declaration = source.classifier(classifier)?;
        if declaration
            .declared_callables
            .keys()
            .any(|name| !declaration.declared_callable_order.contains(name))
        {
            return None;
        }
        for name in declaration
            .declared_callable_order
            .iter()
            .filter(|name| declaration.declared_callables.contains_key(*name))
            .cloned()
        {
            if !names.contains(&name) {
                names.push(name);
            }
        }
        queue.extend(
            crate::symbol_resolver::direct_supertypes(source, current)
                .into_iter()
                .filter(|supertype| {
                    supertype
                        .kotlin_class_internal()
                        .and_then(|classifier| source.classifier(classifier))
                        .is_some_and(|classifier| classifier.is_interface())
                }),
        );
    }
    Some(names)
}

fn interface_owners(source: &dyn SymbolSource, root: Ty) -> Option<HashSet<TypeName>> {
    let mut owners = HashSet::new();
    let mut queue = VecDeque::from([root]);
    while let Some(current) = queue.pop_front() {
        let owner = current.kotlin_class_internal()?;
        if !owners.insert(owner) {
            continue;
        }
        source.classifier(owner)?;
        queue.extend(
            crate::symbol_resolver::direct_supertypes(source, current)
                .into_iter()
                .filter(|supertype| {
                    supertype
                        .kotlin_class_internal()
                        .and_then(|classifier| source.classifier(classifier))
                        .is_some_and(|classifier| classifier.is_interface())
                }),
        );
    }
    Some(owners)
}

fn is_delegated_function(function: &FunctionInfo, interface_owners: &HashSet<TypeName>) -> bool {
    function.kind == FnKind::Member
        || function.kind == FnKind::Extension && interface_owners.contains(&function.callable.owner)
}

fn effective_function<'a>(
    source: &dyn SymbolSource,
    index: &ResolvedModuleIndex,
    root: Ty,
    candidates: impl IntoIterator<Item = &'a FunctionInfo>,
) -> Option<&'a FunctionInfo> {
    let candidates = candidates
        .into_iter()
        .map(|candidate| {
            Some((
                candidate,
                applied_function_result(source, index, root, candidate)?,
            ))
        })
        .collect::<Option<Vec<_>>>()?;
    let nearest = candidates
        .iter()
        .map(|(candidate, _)| candidate.receiver_rank)
        .min()?;
    let nearest = candidates
        .into_iter()
        .filter(|(candidate, _)| candidate.receiver_rank == nearest)
        .collect::<Vec<_>>();
    let mut maximal = nearest
        .iter()
        .copied()
        .filter(|(_, result)| {
            !nearest.iter().copied().any(|(_, other_result)| {
                other_result != *result
                    && crate::symbol_resolver::resolution_subtype(source, other_result, *result)
            })
        })
        .collect::<Vec<_>>();
    let (selected, selected_result) = maximal.pop()?;
    maximal
        .iter()
        .all(|(_, result)| *result == selected_result)
        .then_some(selected)
}

fn effective_property<'a>(
    source: &dyn SymbolSource,
    index: &ResolvedModuleIndex,
    root: Ty,
    candidates: impl IntoIterator<Item = &'a PropertyInfo>,
) -> Option<(&'a PropertyInfo, Option<&'a PropertyInfo>)> {
    let candidates = candidates
        .into_iter()
        .map(|candidate| {
            Some((
                candidate,
                applied_property_signature(source, index, root, candidate)?.1,
            ))
        })
        .collect::<Option<Vec<_>>>()?;
    let nearest = candidates
        .iter()
        .map(|(candidate, _)| candidate.receiver_rank)
        .min()?;
    let nearest = candidates
        .into_iter()
        .filter(|(candidate, _)| candidate.receiver_rank == nearest)
        .collect::<Vec<_>>();
    let mut maximal = nearest
        .iter()
        .copied()
        .filter(|(_, candidate_ty)| {
            !nearest.iter().copied().any(|(_, other_ty)| {
                other_ty != *candidate_ty
                    && crate::symbol_resolver::resolution_subtype(source, other_ty, *candidate_ty)
            })
        })
        .collect::<Vec<_>>();
    let (getter, getter_ty) = maximal.pop()?;
    if !maximal.iter().all(|(_, ty)| *ty == getter_ty) {
        return None;
    }
    let setter = nearest
        .into_iter()
        .find(|(candidate, ty)| candidate.setter.is_some() && *ty == getter_ty)
        .map(|(candidate, _)| candidate);
    Some((getter, setter))
}

fn resolved_types(types: impl IntoIterator<Item = Ty>) -> Option<Box<[ResolvedTy]>> {
    types
        .into_iter()
        .map(ResolvedTy::new)
        .collect::<Result<Vec<_>, _>>()
        .ok()
        .map(Vec::into_boxed_slice)
}

/// Find the applied occurrence of a declaration owner in the delegated interface hierarchy.
///
/// The module index stores the declaration's finalized signature in terms of its owner formals.
/// Member lookup has already selected an applied declaration from this same hierarchy, so closing
/// the delegation must instantiate that stable signature with the corresponding applied owner edge.
/// This is semantic hierarchy state, not a source-name or provisional-type fallback.
fn applied_owner_type(source: &dyn SymbolSource, root: Ty, owner: TypeName) -> Option<Ty> {
    let mut queue = VecDeque::from([root]);
    let mut seen = HashSet::new();
    while let Some(current) = queue.pop_front() {
        let current_owner = current.kotlin_class_internal()?;
        if current_owner == owner {
            return Some(current);
        }
        if !seen.insert(current_owner) {
            continue;
        }
        source.classifier(current_owner)?;
        queue.extend(crate::symbol_resolver::direct_supertypes(source, current));
    }
    None
}

fn stable_member_signature(
    source: &dyn SymbolSource,
    index: &ResolvedModuleIndex,
    root: Ty,
    owner: TypeName,
    declaration: crate::fir::DeclarationId,
) -> Option<(Vec<Ty>, Ty)> {
    let applied_owner = applied_owner_type(source, root, owner)?;
    let classifier = source.classifier(owner)?;
    let bindings = crate::symbol_resolver::classifier_bindings(&classifier, applied_owner);
    let signature = index.signature(declaration)?;
    let parameters = signature
        .parameters
        .iter()
        .map(|parameter| {
            crate::symbol_resolver::specialize_signature_input_type(
                source,
                parameter.get(),
                &bindings,
            )
        })
        .collect();
    let result = crate::symbol_resolver::specialize_signature_output_type(
        source,
        signature.result.get(),
        &bindings,
    );
    Some((parameters, result))
}

fn applied_function_result(
    source: &dyn SymbolSource,
    index: &ResolvedModuleIndex,
    root: Ty,
    function: &FunctionInfo,
) -> Option<Ty> {
    match function.stable_declaration {
        Some(declaration) => {
            stable_member_signature(source, index, root, function.callable.owner, declaration)
                .map(|(_, result)| result)
        }
        None => Some(function.ret.apply(function.callable.ret)),
    }
}

fn applied_property_signature(
    source: &dyn SymbolSource,
    index: &ResolvedModuleIndex,
    root: Ty,
    property: &PropertyInfo,
) -> Option<(Vec<Ty>, Ty)> {
    match property.stable_declaration {
        Some(declaration) => {
            stable_member_signature(source, index, root, property.owner, declaration)
        }
        None => Some((
            property
                .getter
                .params
                .iter()
                .take(property.context_count)
                .copied()
                .collect(),
            property.ty,
        )),
    }
}

fn function_call(
    source: &dyn SymbolSource,
    index: &ResolvedModuleIndex,
    receiver: Ty,
    function: &FunctionInfo,
) -> Option<ResolvedDelegatedCall> {
    let (parameters, result, target, declared_result) = if let Some(declaration) =
        function.stable_declaration
    {
        let callable = index.callable_for_declaration(declaration)?;
        let signature = index.signature(declaration)?;
        let (mut parameters, result) = stable_member_signature(
            source,
            index,
            receiver,
            function.callable.owner,
            declaration,
        )?;
        if let Some(extension_receiver) = callable.shape.extension_receiver {
            let applied_owner = applied_owner_type(source, receiver, function.callable.owner)?;
            let classifier = source.classifier(function.callable.owner)?;
            let bindings = crate::symbol_resolver::classifier_bindings(&classifier, applied_owner);
            let extension_receiver = crate::symbol_resolver::specialize_signature_receiver_type(
                source,
                extension_receiver.get(),
                &bindings,
            );
            parameters.insert(
                callable
                    .shape
                    .context_parameter_count
                    .min(parameters.len() as u32) as usize,
                extension_receiver,
            );
        }
        let mut declared_parameters = signature
            .parameters
            .iter()
            .map(|parameter| *parameter)
            .collect::<Vec<_>>();
        if let Some(extension_receiver) = callable.shape.extension_receiver {
            declared_parameters.insert(
                callable
                    .shape
                    .context_parameter_count
                    .min(declared_parameters.len() as u32) as usize,
                extension_receiver,
            );
        }
        let target = ResolvedDelegatedCallTarget::Module {
            target: ResolvedDelegatedModuleTarget::Function(callable.id),
            owner: function.callable.owner,
            name: function.callable.name.clone().into_boxed_str(),
            parameters: declared_parameters.into_boxed_slice(),
            result: signature.result,
            interface: function.callable.owner_is_interface,
        };
        (parameters, result, target, None)
    } else {
        let parameters = applied_function_parameters(function);
        let result = function.ret.apply(function.callable.ret);
        let target = ResolvedDelegatedCallTarget::External(function.callable.external_identity?);
        let declared_result = function
            .callable
            .declared_ret
            .map(ResolvedTy::new)
            .transpose()
            .ok()?;
        (parameters, result, target, declared_result)
    };
    Some(ResolvedDelegatedCall {
        target,
        receiver: ResolvedTy::new(receiver).ok()?,
        parameters: resolved_types(parameters)?,
        result: ResolvedTy::new(result).ok()?,
        declared_result,
        suspend: function.flags.suspend,
        extension_receiver_parameter: (function.kind == FnKind::Extension)
            .then(|| u32::try_from(function.context_count).ok())
            .flatten(),
    })
}

fn delegated_function_declaration(
    index: &ResolvedModuleIndex,
    function: &FunctionInfo,
) -> Option<ResolvedDelegatedFunctionDeclaration> {
    let (target, parameters, result) = if let Some(declaration) = function.stable_declaration {
        let callable = index.callable_for_declaration(declaration)?;
        let signature = index.signature(declaration)?;
        let mut parameters = signature
            .parameters
            .iter()
            .map(|parameter| parameter.get())
            .collect::<Vec<_>>();
        if let Some(receiver) = callable.shape.extension_receiver {
            parameters.insert(
                callable
                    .shape
                    .context_parameter_count
                    .min(parameters.len() as u32) as usize,
                receiver.get(),
            );
        }
        (
            ResolvedFunctionOverrideTarget::Module(callable.id),
            parameters.into_boxed_slice(),
            signature.result.get(),
        )
    } else {
        let target = ResolvedFunctionOverrideTarget::External(function.callable.external_identity?);
        let parameters = function
            .callable
            .declared_params
            .clone()
            .or_else(|| {
                function.generic_sig.as_ref().map(|signature| {
                    if function.kind == FnKind::Extension {
                        signature.parameters_with_receiver(function.context_count)
                    } else {
                        signature.params.clone().into_boxed_slice()
                    }
                })
            })
            .unwrap_or_else(|| applied_function_parameters(function).into_boxed_slice());
        let result = function
            .callable
            .declared_ret
            .or_else(|| function.generic_sig.as_ref().map(|signature| signature.ret))
            .unwrap_or_else(|| function.ret.apply(function.callable.ret));
        (target, parameters, result)
    };
    Some(ResolvedDelegatedFunctionDeclaration {
        target,
        owner: function.callable.owner,
        parameters: resolved_types(parameters.iter().copied())?,
        result: ResolvedTy::new(result).ok()?,
        interface: function.callable.owner_is_interface,
    })
}

fn property_call(
    index: &ResolvedModuleIndex,
    receiver: Ty,
    property: &PropertyInfo,
    setter: bool,
    context_parameters: &[Ty],
    property_type: Ty,
) -> Option<ResolvedDelegatedCall> {
    let mut parameters = context_parameters.to_vec();
    if setter {
        parameters.push(property_type);
    }
    let result = setter.then_some(Ty::Unit).unwrap_or(property_type);
    let callable = if setter {
        property.setter.as_ref()?
    } else {
        &property.getter
    };
    let (target, declared_result) = if let Some(declaration) = property.stable_declaration {
        let property_id = index.property_for_declaration(declaration)?;
        let signature = index.signature(declaration)?;
        let mut declared_parameters = signature.parameters.to_vec();
        if setter {
            declared_parameters.push(signature.result);
        }
        (
            ResolvedDelegatedCallTarget::Module {
                target: if setter {
                    ResolvedDelegatedModuleTarget::PropertySetter(property_id)
                } else {
                    ResolvedDelegatedModuleTarget::PropertyGetter(property_id)
                },
                owner: property.owner,
                name: if setter {
                    crate::names::property_setter_name(&property.name).into_boxed_str()
                } else {
                    crate::names::property_getter_name(&property.name).into_boxed_str()
                },
                parameters: declared_parameters.into_boxed_slice(),
                result: if setter {
                    ResolvedTy::new(Ty::Unit).ok()?
                } else {
                    signature.result
                },
                interface: true,
            },
            None,
        )
    } else {
        (
            ResolvedDelegatedCallTarget::External(callable.external_identity?),
            callable
                .declared_ret
                .map(ResolvedTy::new)
                .transpose()
                .ok()?,
        )
    };
    Some(ResolvedDelegatedCall {
        target,
        receiver: ResolvedTy::new(receiver).ok()?,
        parameters: resolved_types(parameters)?,
        result: ResolvedTy::new(result).ok()?,
        declared_result,
        suspend: false,
        extension_receiver_parameter: None,
    })
}

fn delegation_members(
    source: &dyn SymbolSource,
    index: &ResolvedModuleIndex,
    delegating_classifier: TypeName,
    interface: Ty,
) -> Option<Box<[ResolvedDelegatedMember]>> {
    let interface_owners = interface_owners(source, interface)?;
    let own = Ty::obj_name(delegating_classifier);
    let mut members = Vec::new();
    for name in hierarchy_names(source, interface)? {
        let delegated = crate::symbol_resolver::members_in_hierarchy(source, interface, &name);
        let own_callables = crate::symbol_resolver::members_in_hierarchy(source, own, &name);
        let own_function_slots = own_callables
            .functions()
            .iter()
            .filter(|function| {
                function.receiver_rank == 0
                    && (function.kind == FnKind::Member
                        || function.kind == FnKind::Extension
                            && function.callable.owner == delegating_classifier)
            })
            .map(function_slot)
            .collect::<HashSet<_>>();
        let own_property = own_callables
            .properties()
            .iter()
            .any(|property| property.receiver_rank == 0 && property.kind == PropKind::Member);

        let mut slots: Vec<(FunctionSlot, Vec<&FunctionInfo>)> = Vec::new();
        for function in delegated
            .functions()
            .iter()
            .filter(|function| is_delegated_function(function, &interface_owners))
        {
            let slot = function_slot(function);
            if let Some((_, candidates)) =
                slots.iter_mut().find(|(candidate, _)| *candidate == slot)
            {
                candidates.push(function);
            } else {
                slots.push((slot, vec![function]));
            }
        }
        for (slot, candidates) in slots {
            if own_function_slots.contains(&slot) {
                continue;
            }
            let function = effective_function(source, index, interface, candidates)?;
            let call = function_call(source, index, interface, function);
            let overridden = delegated_function_declaration(index, function)?;
            let type_parameters = match function.generic_sig.as_ref() {
                Some(signature) => {
                    if signature.formals.len() != signature.formal_bounds.len() {
                        return None;
                    }
                    signature
                        .formals
                        .iter()
                        .zip(&signature.formal_bounds)
                        .map(|(semantic_name, bounds)| {
                            Some(ResolvedDelegatedTypeParameter {
                                name: crate::types::type_parameter_source_name(semantic_name)
                                    .into(),
                                semantic_name: semantic_name.clone().into_boxed_str(),
                                bounds: resolved_types(bounds.iter().copied())?,
                            })
                        })
                        .collect::<Option<Vec<_>>>()
                        .map(Vec::into_boxed_slice)?
                }
                None => Box::default(),
            };
            members.push(ResolvedDelegatedMember::Function(
                ResolvedDelegatedFunction {
                    name: function.callable.name.clone().into_boxed_str(),
                    type_parameters,
                    overridden,
                    call: call?,
                },
            ));
        }

        if own_property {
            continue;
        }
        let property_candidates = delegated
            .properties()
            .iter()
            // Java accessor-derived properties are an additional source facet of the same JVM
            // method, not another interface declaration. The function slot already forwards that
            // exact external identity (`CharSequence.isEmpty`/`length`); emitting a Kotlin property
            // accessor as well would duplicate or rename the physical method.
            .filter(|property| property.kind == PropKind::Member && !property.accessor_derived)
            .collect::<Vec<_>>();
        if property_candidates.is_empty() {
            continue;
        }
        let (getter, setter) = effective_property(source, index, interface, property_candidates)?;
        let (context_parameters, property_type) =
            applied_property_signature(source, index, interface, getter)?;
        if getter.context_param_names.len() != context_parameters.len() {
            return None;
        }
        members.push(ResolvedDelegatedMember::Property(
            ResolvedDelegatedProperty {
                name: getter.name.clone().into_boxed_str(),
                ty: ResolvedTy::new(property_type).ok()?,
                context_parameters: getter
                    .context_param_names
                    .iter()
                    .zip(&context_parameters)
                    .map(|(name, ty)| {
                        Some(ResolvedDelegatedContextParameter {
                            name: name.clone().into_boxed_str(),
                            ty: ResolvedTy::new(*ty).ok()?,
                        })
                    })
                    .collect::<Option<Vec<_>>>()?
                    .into_boxed_slice(),
                getter: property_call(
                    index,
                    interface,
                    getter,
                    false,
                    &context_parameters,
                    property_type,
                )?,
                setter: match setter {
                    Some(property) => Some(property_call(
                        index,
                        interface,
                        property,
                        true,
                        &context_parameters,
                        property_type,
                    )?),
                    None => None,
                },
            },
        ));
    }
    Some(members.into_boxed_slice())
}

pub(super) fn resolve_interface_delegation(
    table: &SymbolTable,
    index: &ResolvedModuleIndex,
    source_file: u32,
    delegating_classifier: TypeName,
    interface: Ty,
    source: ResolvedInterfaceDelegateSource,
) -> Option<ResolvedInterfaceDelegation> {
    let module = crate::module_symbols::ModuleSymbols::for_file(table, source_file);
    let symbols = CompositeSource::new(vec![
        &module as &dyn SymbolSource,
        table.libraries.as_ref() as &dyn SymbolSource,
    ]);
    let members = delegation_members(&symbols, index, delegating_classifier, interface)?;
    Some(ResolvedInterfaceDelegation {
        interface: ResolvedTy::new(interface).ok()?,
        source,
        members,
    })
}

pub(super) fn resolve_streamed_interface_delegation(
    platform: &dyn crate::libraries::SemanticPlatform,
    index: &ResolvedModuleIndex,
    source_file: u32,
    delegating_classifier: TypeName,
    interface: Ty,
    source: ResolvedInterfaceDelegateSource,
) -> Option<ResolvedInterfaceDelegation> {
    let module = crate::fir::StreamedModuleSymbols::for_file(index, source_file);
    let symbols = CompositeSource::new(vec![
        &module as &dyn SymbolSource,
        platform as &dyn SymbolSource,
    ]);
    let members = delegation_members(&symbols, index, delegating_classifier, interface)?;
    Some(ResolvedInterfaceDelegation {
        interface: ResolvedTy::new(interface).ok()?,
        source,
        members,
    })
}
