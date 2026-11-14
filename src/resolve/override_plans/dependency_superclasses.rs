//! The dispatch shape of the dependency classes the module's classes extend, frozen while the
//! declaration providers are live ([`crate::fir::ResolvedDependencySuperclass`]).
//!
//! A dependency class's members are numbered in every module that extends it, so its shape is
//! read from its own declarations: its superclass, the members a subclass overrides or reaches by
//! dispatch (open, abstract and overriding ones) in declaration order, and their override edges.
//! The edges are selected by the rule that selects a module class's
//! ([`super::append_function_override_edges`], [`super::append_property_override_edges`]), over
//! the dependency class's own hierarchy applied to its own formals.

use crate::fir::{
    ExternalCallableId, ResolvedAppliedClassifier, ResolvedDependencyMember,
    ResolvedDependencySuperclass, ResolvedFunctionOverrideTarget, ResolvedModuleIndex,
    ResolvedPropertyOverrideTarget, ResolvedTy,
};
use crate::fx_hash::HashSet;
use crate::libraries::{FunctionInfo, LibraryType, PropertyInfo};
use crate::symbol_source::SymbolSource;
use crate::types::TypeName;

use super::{
    append_function_override_edges, append_property_override_edges,
    declaration_parameters_with_receiver, declared_functions, declared_properties,
    dependency_declaration_signature, function_parameter_identities, resolved_types,
    with_function_supertypes, OverridingFunction, PropertyImplementation,
};

/// The dependency classes among the hierarchies of `module_classes`: every class ancestor the
/// module does not declare, other than `kotlin.Any`, with its dispatch shape.
pub(super) fn dependency_superclasses(
    index: &ResolvedModuleIndex,
    source: &dyn SymbolSource,
    module_classes: &[crate::fir::DeclarationId],
    declared_by_module: impl Fn(TypeName) -> bool,
) -> Vec<ResolvedDependencySuperclass> {
    let mut seen = HashSet::default();
    let mut superclasses = Vec::new();
    for &classifier in module_classes {
        for ancestor in index.classifier_hierarchy(classifier).unwrap_or_default() {
            let name = ancestor.classifier;
            if ancestor.depth == 0
                || name == crate::types::wk::any()
                || declared_by_module(name)
                || !seen.insert(name)
            {
                continue;
            }
            let Some(declaration) = source.classifier(name) else {
                continue;
            };
            if declaration.is_interface() || declaration.is_object() {
                continue;
            }
            superclasses.push(dependency_superclass(index, source, name, &declaration));
        }
    }
    superclasses
}

/// The dispatch shape `declaration`, the dependency class `classifier`, publishes.
fn dependency_superclass(
    index: &ResolvedModuleIndex,
    source: &dyn SymbolSource,
    classifier: TypeName,
    declaration: &LibraryType,
) -> ResolvedDependencySuperclass {
    let root = declaration.declared_self_type(classifier);
    let hierarchy = with_function_supertypes(
        source,
        &crate::symbol_resolver::applied_hierarchy(source, root)
            .into_iter()
            .filter_map(|(classifier, applied, depth)| {
                Some(ResolvedAppliedClassifier {
                    classifier,
                    applied: ResolvedTy::new(applied).ok()?,
                    depth: u32::try_from(depth).ok()?,
                })
            })
            .collect::<Vec<_>>(),
    );
    let superclass = declaration
        .supertypes
        .iter()
        .find(|&supertype| {
            source
                .classifier(supertype)
                .is_some_and(|record| !record.is_interface())
        })
        .filter(|&superclass| superclass != crate::types::wk::any());
    let mut members = Vec::new();
    let mut function_overrides = Vec::new();
    let mut property_overrides = Vec::new();
    let mut seen_properties = HashSet::default();
    for name in &declaration.declared_callable_order {
        for function in declared_functions(source, root, name) {
            let Some(callable) = function.callable.external_identity else {
                continue;
            };
            // A member is dispatched when a subclass can override it or it overrides another,
            // which its override edges, selected as a module member's are, decide.
            let first = function_overrides.len();
            append_function_override_edges(
                index,
                source,
                classifier,
                name,
                &overriding_function(callable, &function),
                &hierarchy,
                &mut function_overrides,
            );
            if function.flags.is_final && function_overrides.len() == first {
                continue;
            }
            let (parameters, result) = dependency_declaration_signature(source, callable);
            members.push(ResolvedDependencyMember::Function {
                callable,
                parameters: resolved_types(
                    parameters.iter().copied(),
                    "dependency member parameters",
                ),
                result: super::resolved_ty(result, "dependency member result"),
                suspend: function.flags.suspend,
            });
        }
        for member_extensions in [false, true] {
            for property in declared_properties(source, root, name, member_extensions) {
                let Some(getter) = property.getter.external_identity else {
                    continue;
                };
                let Some(implementation) = overriding_property(classifier, name, getter, &property)
                else {
                    continue;
                };
                let first = property_overrides.len();
                append_property_override_edges(
                    index,
                    source,
                    &implementation,
                    &hierarchy,
                    &mut seen_properties,
                    &mut property_overrides,
                );
                if property.is_final && property_overrides.len() == first {
                    continue;
                }
                let (parameters, value) = dependency_declaration_signature(source, getter);
                members.push(ResolvedDependencyMember::Property {
                    getter,
                    setter: property.setter.is_some(),
                    accessor_parameters: resolved_types(
                        parameters.iter().copied(),
                        "dependency accessor parameters",
                    ),
                    value: super::resolved_ty(value, "dependency property value"),
                });
            }
        }
    }
    function_overrides.sort_by_key(|edge| edge.depth);
    property_overrides.sort_by_key(|edge| edge.depth);
    ResolvedDependencySuperclass {
        classifier,
        superclass,
        members: members.into_boxed_slice(),
        function_overrides: function_overrides.into_boxed_slice(),
        property_overrides: property_overrides.into_boxed_slice(),
    }
}

/// The overriding side of an edge whose implementation is the dependency function `function`.
fn overriding_function(
    callable: ExternalCallableId,
    function: &FunctionInfo,
) -> OverridingFunction {
    let parameters = declaration_parameters_with_receiver(function);
    let (formals, formal_bounds) = function
        .generic_sig
        .as_ref()
        .map(|signature| (signature.formals.clone(), signature.formal_bounds.clone()))
        .unwrap_or_default();
    OverridingFunction {
        target: ResolvedFunctionOverrideTarget::External(callable),
        parameter_identities: function_parameter_identities(function, parameters.len()),
        formals,
        formal_bounds,
        receiver: function
            .semantic_receiver()
            .filter(|_| function.is_extension()),
        context_count: function.context_count,
        result: function
            .ret
            .apply(function.callable.ret)
            .canonical_semantic(),
        suspend: function.flags.suspend,
        parameters,
    }
}

/// The overriding side of an edge whose implementation is the dependency property `property`,
/// by its getter's identity; `None` when its provider publishes a type the frozen FIR cannot hold.
fn overriding_property<'a>(
    owner: TypeName,
    name: &'a str,
    getter: ExternalCallableId,
    property: &PropertyInfo,
) -> Option<PropertyImplementation<'a>> {
    Some(PropertyImplementation {
        owner,
        name,
        target: ResolvedPropertyOverrideTarget::External(getter),
        ty: ResolvedTy::new(property.ty.canonical_semantic()).ok()?,
        formals: property.formals.clone(),
        formal_bounds: property
            .getter
            .generic_sig
            .as_ref()
            .map(|signature| signature.formal_bounds.clone())
            .unwrap_or_default(),
        context_parameters: property
            .getter
            .params
            .get(..property.context_count)
            .unwrap_or_default()
            .to_vec(),
        // A member property's provider receiver is its dispatch receiver; only a member
        // extension's declared receiver is part of the slot it overrides.
        receiver: property
            .receiver
            .filter(|_| property.kind == crate::libraries::PropKind::MemberExtension)
            .map(|receiver| ResolvedTy::new(receiver.canonical_semantic()))
            .transpose()
            .ok()?,
        mutable: property.setter.is_some(),
    })
}
