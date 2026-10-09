//! Source-level argument compatibility shared by every callable origin.
//!
//! This module owns the boundary between raw argument shapes, SAM/function adaptation, and the
//! common source hierarchy. It deliberately knows nothing about JVM descriptors or which provider
//! supplied a declaration.

use crate::libraries::SemanticPlatform;
use crate::symbol_source::SymbolSource;
use crate::types::Ty;

use super::{
    instantiate_slot, semantic_arg_assignable, semantic_sam_signature, GSigBinds, SourceOracle,
    TypePosition, UnboundSpecialization,
};

/// If `sig` is a function type, return its partially substituted semantic input types. Unbound
/// formals remain symbolic so a postponed lambda body can still contribute inference evidence.
pub(crate) fn function_input_types(
    source: &dyn SymbolSource,
    sig: Ty,
    binds: &GSigBinds,
) -> Vec<Ty> {
    match sig.non_null() {
        Ty::Fun(signature) => signature
            .params
            .iter()
            .map(|parameter| {
                instantiate_slot(
                    source,
                    None,
                    *parameter,
                    binds,
                    TypePosition::Out,
                    UnboundSpecialization::Preserve,
                )
            })
            .collect(),
        _ => Vec::new(),
    }
}

/// Whether an argument fits a parameter in erased Kotlin terms.
pub(crate) fn arg_fits(parameter: &Ty, argument: &Ty) -> bool {
    parameter == argument
        || matches!(parameter, Ty::Obj(name, _)
            if (crate::types::same(*name, crate::types::wk::any())
                || crate::types::same(*name, crate::types::wk::java_object()))
                && !matches!(argument, Ty::Null | Ty::Nullable(_)))
        || matches!((parameter.fun_arity(), argument.fun_arity()), (Some(expected), Some(actual)) if expected == actual)
        || matches!((parameter, argument), (Ty::Obj(expected, _), Ty::Obj(actual, _)) if expected == actual)
}

/// Whether a function-shaped argument can adapt to a functional-interface parameter.
pub(crate) fn sam_arg_matches(
    platform: &dyn SemanticPlatform,
    source: &dyn SymbolSource,
    parameter: Ty,
    argument: Ty,
) -> bool {
    let Some(sam) = semantic_sam_signature(source, parameter) else {
        return false;
    };
    if argument == Ty::Error {
        return sam.params.len() <= 1;
    }
    let Some(arity) = argument.fun_arity() else {
        return false;
    };
    if sam.params.len() != usize::from(arity) {
        return false;
    }
    let Some(result) = argument.fun_ret() else {
        return false;
    };
    sam_return_matches(platform, source, sam.ret, result)
}

pub(crate) fn sam_return_matches(
    _platform: &dyn SemanticPlatform,
    source: &dyn SymbolSource,
    expected: Ty,
    actual: Ty,
) -> bool {
    if expected == Ty::Unit || matches!(actual, Ty::Error | Ty::Nothing) {
        return true;
    }
    let expected = match expected {
        Ty::TyParam(_, bound) => *bound,
        expected => expected,
    };
    semantic_arg_assignable(source, &expected, &actual)
}

pub(super) fn arg_fits_platform(
    platform: &dyn SemanticPlatform,
    parameter: &Ty,
    argument: &Ty,
) -> bool {
    arg_fits(parameter, argument)
        || parameter
            .fun_arity()
            .zip(platform.function_like_arity(*argument))
            .is_some_and(|(expected, actual)| usize::from(expected) == actual)
}

pub(super) fn arg_fits_source(
    platform: &dyn SemanticPlatform,
    source: &dyn SymbolSource,
    parameter: &Ty,
    argument: &Ty,
) -> bool {
    arg_fits_platform(platform, parameter, argument)
        || semantic_arg_assignable(source, parameter, argument)
        // `null` may constrain a callable-owned formal to `Nothing?`; a fixed enclosing bare type
        // parameter is rejected later by the ordinary checker with its precise diagnostic.
        || (*argument == Ty::Null && matches!(parameter.non_null(), Ty::TyParam(..)))
}

/// Whether a parameter can host an implicitly typed lambda before its body is checked.
pub(super) fn untyped_lambda_pertinent(
    platform: &dyn SemanticPlatform,
    source: &dyn SymbolSource,
    parameter: Ty,
) -> bool {
    let shape = parameter.non_null();
    shape.fun_arity().is_some()
        || matches!(shape, Ty::TyParam(..))
        || shape.is_erased_top()
        || (0..=1)
            .filter_map(|arity| platform.function_type(arity))
            .any(|function| semantic_arg_assignable(source, &shape, &function))
        || sam_arg_matches(platform, source, parameter, Ty::Error)
}

/// The common source-hierarchy subtype relation used by resolution and checking.
pub(crate) fn resolution_subtype(source: &dyn SymbolSource, sub: Ty, sup: Ty) -> bool {
    crate::assignable::is_subtype(
        &crate::assignable::TyCtx::new(),
        &SourceOracle(source),
        sub,
        sup,
    )
}
