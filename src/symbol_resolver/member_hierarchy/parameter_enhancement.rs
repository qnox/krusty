//! Java parameter enhancement from overridden declarations.
//!
//! kotlinc enhances a Java member's value parameters with the nullability of the declarations they
//! override, as it does its result (see [`super::result_enhancement`]): a Java parameter with no
//! nullability qualifier of its own (`String!`) that overrides a parameter fixed not-null is that
//! not-null type, so a Java class implementing `interface I { fun f(x: String) }` rejects `null`
//! for `f`.
//!
//! An overridden parameter that is a type variable of its owner reads through the overriding
//! class's supertype argument, which a Java supertype writes as a flexible `T!`. That occurrence is
//! not rigid, so a parameter declared over a type variable contributes no qualifier here.

use super::result_enhancement::overrides;
use crate::libraries::{FunctionInfo, FunctionSet};
use crate::symbol_source::SymbolSource;
use crate::types::TyKind;

/// Whether `inherited` fixes its parameter `index` not-null: the declaration states the type
/// itself, it is neither nullable nor flexible, and the declaration does not accept a platform
/// `null` there.
fn fixes_parameter_not_null(
    source: &dyn SymbolSource,
    inherited: &FunctionInfo,
    index: usize,
) -> bool {
    // A generic owner's declaration reads its parameter through the overriding class's supertype
    // argument, whose flexibility is that occurrence's own; only a declaration whose owner has no
    // type variable states its parameter itself.
    let owner_is_generic = source
        .classifier(inherited.callable.owner)
        .is_none_or(|owner| !owner.type_params().is_empty());
    if owner_is_generic {
        return false;
    }
    let Some(parameter) = inherited.semantic_params().get(index).copied() else {
        return false;
    };
    let declared = inherited
        .generic_sig
        .as_ref()
        .and_then(|signature| signature.params.get(index).copied())
        .unwrap_or(parameter);
    !declared.mentions_ty_param()
        && !inherited
            .call_sig
            .platform_nullable_params
            .get(index)
            .copied()
            .unwrap_or(false)
        && !parameter.is_nullable()
        && !matches!(
            parameter.kind(),
            TyKind::PlatformNullable(_) | TyKind::Error | TyKind::Pending
        )
        && parameter.is_reference()
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
            if overridden
                .iter()
                .any(|inherited| fixes_parameter_not_null(source, inherited, index))
            {
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
                        *parameter = parameter.platform_lower_bound();
                    }
                }
                let leading = implementation
                    .callable
                    .params
                    .len()
                    .saturating_sub(implementation.semantic_params().len());
                if let Some(parameter) = implementation.callable.params.get_mut(leading + index) {
                    *parameter = parameter.platform_lower_bound();
                }
            }
        }
    }
}
