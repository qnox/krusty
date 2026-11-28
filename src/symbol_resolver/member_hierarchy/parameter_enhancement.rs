//! Java parameter enhancement from overridden declarations.
//!
//! kotlinc enhances a Java member's value parameters with the nullability of the declarations they
//! override, as it does its result (see [`super::result_enhancement`]): a Java parameter with no
//! nullability qualifier of its own (`String!`) that overrides a parameter fixed not-null is that
//! not-null type, so a Java class implementing `interface I { fun f(x: String) }` rejects `null`
//! for `f`. The qualifier is the head nullability of the overridden parameter as the receiver sees
//! it, and the overridden declarations must agree: parameters are contravariant, so one nullable
//! declaration keeps the Java parameter flexible.
//!
//! A parameter whose declared head is a type variable of the overridden owner reads that variable
//! through the supertype edges between the overriding class and the owner. An edge a Java class
//! declares writes its arguments flexibly (`KG<String!>`), so the occurrence is not rigid and
//! contributes no qualifier; a Kotlin class's edge states its arguments, and keeps the argument
//! rigid.

use super::result_enhancement::{head_qualifier, overrides, NullabilityQualifier};
use crate::fx_hash::HashSet;
use crate::libraries::{FunctionInfo, FunctionSet};
use crate::symbol_source::SymbolSource;
use crate::types::{Ty, TyKind, TypeName};

/// Whether a supertype argument is Java's flexible occurrence: written flexibly, or a variable
/// whose bound is the platform one.
fn argument_is_flexible(argument: Ty) -> bool {
    let argument = argument.projection_inner().unwrap_or(argument);
    argument.is_platform_nullable()
        || matches!(argument.kind(), TyKind::TyParam(_, bound) if bound.is_platform_nullable())
}

/// Whether `to` is reached from `from` through supertype edges that all state their arguments
/// rigidly.
fn reaches_through_rigid_edges(source: &dyn SymbolSource, from: TypeName, to: TypeName) -> bool {
    let mut pending = vec![from];
    let mut seen = HashSet::default();
    while let Some(current) = pending.pop() {
        if current == to {
            return true;
        }
        if !seen.insert(current) {
            continue;
        }
        let Some(classifier) = source.classifier(current) else {
            continue;
        };
        for template in &classifier.supertype_templates {
            if let TyKind::Obj(supertype, arguments) = template.kind() {
                if !arguments.iter().copied().any(argument_is_flexible) {
                    pending.push(supertype);
                }
            }
        }
    }
    false
}

/// Whether the overriding `implementation`'s class reaches the declaration `inherited` overrides
/// through rigid edges. The declaration is the one its owner states, a mapped builtin's Kotlin
/// declaration rather than the platform class that realizes it.
fn reaches_rigidly(
    source: &dyn SymbolSource,
    implementation: &FunctionInfo,
    inherited: &FunctionInfo,
) -> bool {
    reaches_through_rigid_edges(
        source,
        implementation.callable.owner,
        super::function_declaration_identity(inherited).owner,
    )
}

/// The head qualifier `inherited` contributes to its parameter `index`, as the overriding
/// `implementation` sees it; `None` when the occurrence carries none. A head that is a type
/// variable of the overridden owner is stated by the supertype edges that reach the owner: a Java
/// edge writes its argument flexibly, so only an owner reached through rigid edges qualifies it.
fn parameter_qualifier(
    source: &dyn SymbolSource,
    implementation: &FunctionInfo,
    inherited: &FunctionInfo,
    index: usize,
) -> Option<NullabilityQualifier> {
    let parameter = inherited.semantic_params().get(index).copied()?;
    // A primitive parameter is a value, not a reference with a nullability to enhance.
    if !parameter.is_reference() {
        return None;
    }
    let declared_flexible = inherited
        .call_sig
        .platform_nullable_params
        .get(index)
        .copied()
        .unwrap_or(false);
    let qualifier = head_qualifier(parameter, declared_flexible)?;
    if qualifier == NullabilityQualifier::NotNull
        && head_is_owner_variable(inherited, index)
        && !reaches_rigidly(source, implementation, inherited)
    {
        return None;
    }
    Some(qualifier)
}

/// Whether the declared head of `inherited`'s parameter `index` is a type variable of its owner. The
/// head is read from the declaration as its owner wrote it, not from the receiver's substitution of
/// it: a variable of the owner stays one however the receiver binds it.
fn head_is_owner_variable(inherited: &FunctionInfo, index: usize) -> bool {
    super::function_declaration_identity(inherited)
        .params
        .get(index)
        .is_none_or(|declared| matches!(declared.kind(), TyKind::TyParam(..)))
}

/// A type argument without its platform nullability, through a projection.
fn rigid_argument(argument: Ty) -> Ty {
    match argument.kind() {
        TyKind::InProjection(bound) => Ty::in_projection(bound.platform_lower_bound()),
        TyKind::OutProjection(bound) => Ty::out_projection(bound.platform_lower_bound()),
        _ => argument.platform_lower_bound(),
    }
}

/// The enhanced form of the flexible Java `parameter` that the not-null `inherited` parameter
/// fixes. The overridden head replaces the Java one, so a flexible collection takes the overridden
/// mutability; the Java arguments stay flexible unless the overridden occurrence is rigid.
fn enhanced_parameter(parameter: Ty, inherited: Ty, rigid: bool) -> Ty {
    // A variable the receiver bound the overridden owner's own variable to is the parameter's type
    // as it stands: it carries the variable's nullability, not a not-null one.
    if inherited.is_ty_param() {
        return inherited;
    }
    let lower = parameter.platform_lower_bound();
    match (lower.kind(), inherited.non_null().kind()) {
        (TyKind::Obj(_, arguments), TyKind::Obj(head, _)) => {
            let arguments = if rigid {
                arguments.iter().copied().map(rigid_argument).collect()
            } else {
                arguments.to_vec()
            };
            Ty::obj_args_name(head, &arguments)
        }
        _ => lower,
    }
}

/// Make each flexible Java parameter (`String!`) that an overridden declaration fixes not-null
/// reject `null`.
pub(super) fn enhance_overriding_flexible_parameters(
    source: &dyn SymbolSource,
    functions: &mut FunctionSet,
) {
    if !functions
        .overloads
        .iter()
        .any(|function| function.call_sig.platform_nullable_params.contains(&true))
    {
        return;
    }
    let declarations = functions.overloads.clone();
    for implementation in &mut functions.overloads {
        let flexible = implementation
            .call_sig
            .platform_nullable_params
            .iter()
            .enumerate()
            .filter(|(_, accepts_null)| **accepts_null)
            .map(|(index, _)| index)
            .collect::<Vec<_>>();
        if flexible.is_empty() {
            continue;
        }
        let overridden = declarations
            .iter()
            .filter(|inherited| overrides(source, implementation, inherited))
            .collect::<Vec<_>>();
        for index in flexible {
            let qualifiers = overridden
                .iter()
                .filter_map(|inherited| {
                    parameter_qualifier(source, implementation, inherited, index)
                })
                .collect::<Vec<_>>();
            if qualifiers.contains(&NullabilityQualifier::NotNull)
                && !qualifiers.contains(&NullabilityQualifier::Nullable)
            {
                let Some((fixed, rigid)) = overridden.iter().find_map(|inherited| {
                    (parameter_qualifier(source, implementation, inherited, index)
                        == Some(NullabilityQualifier::NotNull))
                    .then(|| {
                        (
                            inherited.semantic_params()[index],
                            reaches_rigidly(source, implementation, inherited),
                        )
                    })
                }) else {
                    continue;
                };
                crate::trace_compiler!(
                    "resolve",
                    "enhanced overriding parameter owner={:?} name={} index={index}",
                    implementation.callable.owner,
                    implementation.callable.name,
                );
                implementation
                    .call_sig
                    .platform_nullable_params
                    .set(index, false);
                // The declared parameter types follow the semantic parameters, which the
                // extension receiver and context parameters precede on the callable.
                if let Some(signature) = &mut implementation.generic_sig {
                    if let Some(parameter) = signature.params.get_mut(index) {
                        *parameter = enhanced_parameter(*parameter, fixed, rigid);
                    }
                }
                let leading = implementation
                    .callable
                    .params
                    .len()
                    .saturating_sub(implementation.semantic_params().len());
                if let Some(parameter) = implementation.callable.params.get_mut(leading + index) {
                    *parameter = enhanced_parameter(*parameter, fixed, rigid);
                }
            }
        }
    }
}
