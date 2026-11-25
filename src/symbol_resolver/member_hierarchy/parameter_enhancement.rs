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

use super::result_enhancement::overrides;
use crate::fx_hash::HashSet;
use crate::libraries::{FunctionInfo, FunctionSet};
use crate::symbol_source::SymbolSource;
use crate::types::{Ty, TyKind, TypeName};

/// The head nullability qualifier one overridden declaration contributes to a parameter.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ParameterQualifier {
    NotNull,
    Nullable,
}

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

/// Whether `to` is reached from `from` with its type variables stated rigidly: through edges that
/// all state their arguments rigidly, or, for a pure implementation, through its pure supertype,
/// which reaches every mapped builtin collection declaration.
fn reaches_rigidly(
    source: &dyn SymbolSource,
    implementation: &FunctionInfo,
    inherited: &FunctionInfo,
) -> bool {
    let (from, to) = (implementation.callable.owner, inherited.callable.owner);
    reaches_through_rigid_edges(source, from, to)
        || source
            .classifier(from)
            .is_some_and(|classifier| classifier.purely_implemented.is_some())
            && source
                .classifier(super::function_declaration_identity(inherited).owner)
                .is_some_and(|classifier| classifier.mapped_builtin)
}

/// The head qualifier `inherited` contributes to its parameter `index`, as the overriding
/// `implementation` sees it; `None` when the occurrence carries none.
fn parameter_qualifier(
    source: &dyn SymbolSource,
    implementation: &FunctionInfo,
    inherited: &FunctionInfo,
    index: usize,
) -> Option<ParameterQualifier> {
    let parameter = inherited.semantic_params().get(index).copied()?;
    if parameter.is_nullable() {
        return Some(ParameterQualifier::Nullable);
    }
    if inherited
        .call_sig
        .platform_nullable_params
        .get(index)
        .copied()
        .unwrap_or(false)
        || matches!(
            parameter.kind(),
            TyKind::PlatformNullable(_) | TyKind::Error | TyKind::Pending
        )
        || !parameter.is_reference()
    {
        return None;
    }
    if head_is_owner_variable(inherited, index)
        && !reaches_rigidly(source, implementation, inherited)
    {
        return None;
    }
    Some(ParameterQualifier::NotNull)
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
fn enhanced_parameter(parameter: Ty, inherited: Ty, rigid: bool, variable_head: bool) -> Ty {
    // A variable the receiver bound the overridden owner's own variable to is the parameter's type
    // as it stands: it carries the variable's nullability, not a not-null one.
    if inherited.is_ty_param() {
        return inherited;
    }
    // A receiver whose arguments are not yet known reads the overridden owner's variable as its
    // erasure, `Any`. That is the variable unbound, which the parameter keeps, not the not-null
    // `Any` an argument would fix.
    if variable_head && inherited == Ty::obj_name(crate::types::wk::any()) {
        return parameter;
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
            if qualifiers.contains(&ParameterQualifier::NotNull)
                && !qualifiers.contains(&ParameterQualifier::Nullable)
            {
                let Some((fixed, rigid, variable_head)) = overridden.iter().find_map(|inherited| {
                    (parameter_qualifier(source, implementation, inherited, index)
                        == Some(ParameterQualifier::NotNull))
                    .then(|| {
                        (
                            inherited.semantic_params()[index],
                            reaches_rigidly(source, implementation, inherited),
                            head_is_owner_variable(inherited, index),
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
                        *parameter = enhanced_parameter(*parameter, fixed, rigid, variable_head);
                    }
                }
                let leading = implementation
                    .callable
                    .params
                    .len()
                    .saturating_sub(implementation.semantic_params().len());
                if let Some(parameter) = implementation.callable.params.get_mut(leading + index) {
                    *parameter = enhanced_parameter(*parameter, fixed, rigid, variable_head);
                }
            }
        }
    }
}
