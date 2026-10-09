//! Generic callable signatures published from source declarations.
//!
//! Source and compact headers share one publication path so inferred return parameters, declared
//! bounds, receiver types, and vararg element wrapping cannot diverge between inspection and the
//! streaming compiler.

use std::collections::HashMap;

use crate::ast::{Expr, File, FunBody, FunDecl};
use crate::diag::DiagSink;
use crate::libraries::GenericSig;
use crate::types::Ty;

use super::{
    legacy_callable_header, semantic_value_parameter_ty, ty_of_ref, ClassNames,
    StreamedCallableHeader, StreamedResultKind, TParams,
};

/// The function's own type parameter that an inferred return type resolves to, read off the
/// declaration rather than the erased inference. This covers both a returned value parameter
/// (`fun <T> id(x: T) = x`) and invoking a function parameter whose declared return is the type
/// parameter (`fun <T> use(f: () -> T) = f()`). In both cases the declaration says `T`; publishing
/// the body's erased `Any?` would discard the constraint needed by callers.
pub(super) fn inferred_return_type_parameter(file: &File, function: &FunDecl) -> Option<String> {
    let header = legacy_callable_header(function);
    inferred_return_type_parameter_from_header(file, function, &header)
}

pub(super) fn inferred_return_type_parameter_from_header(
    file: &File,
    function: &FunDecl,
    header: &StreamedCallableHeader,
) -> Option<String> {
    if header.result != StreamedResultKind::Inferred || header.type_parameters.is_empty() {
        return None;
    }
    let FunBody::Expr(body) = &function.body else {
        return None;
    };
    // Production may have released this ordinary expression arena after compact signature
    // extraction. In that path the signature graph is the sole owner of inferred return identity;
    // this inspection refinement must not reopen (or index) the discarded body.
    let body = file.expr_arena.get(body.0 as usize)?;
    let returned_type = match body {
        Expr::Name(name) if name == "this" => header.receiver.as_ref()?,
        Expr::Name(name) => {
            let parameter = function
                .params
                .iter()
                .position(|parameter| &parameter.name == name)?;
            &header.parameters.get(parameter)?.ty
        }
        Expr::Call { callee, .. } => {
            let Expr::Name(name) = file.expr_arena.get(callee.0 as usize)? else {
                return None;
            };
            let parameter = function
                .params
                .iter()
                .position(|parameter| &parameter.name == name)?;
            let parameter = &header.parameters.get(parameter)?.ty;
            if parameter.name != "<fun>" && parameter.fun_params.is_empty() {
                return None;
            }
            parameter.arg.as_deref()?
        }
        _ => return None,
    };
    header
        .type_parameters
        .iter()
        .find(|type_parameter| {
            *type_parameter == &returned_type.name && returned_type.targs.is_empty()
        })
        .cloned()
}

pub(super) fn source_generic_signature_from_tparams(
    function: &FunDecl,
    type_params: &TParams,
    receiver: Option<Ty>,
    params: Vec<Ty>,
    resolved_ret: Ty,
    inferred_ret_tparam: Option<String>,
    formal_bounds: Vec<Vec<Ty>>,
) -> GenericSig {
    let header = legacy_callable_header(function);
    let bindings = header
        .type_parameters
        .iter()
        .map(|source| (source.clone(), type_params.bound(source)))
        .collect::<HashMap<_, _>>();
    let ret = if header.explicit_result.is_some() {
        resolved_ret
    } else {
        crate::symbol_resolver::ty_subst_keep_unbound(resolved_ret, &bindings)
    };
    let ret = match (header.result, inferred_ret_tparam) {
        (StreamedResultKind::Inferred, Some(name)) => type_params.bound(&name),
        _ => ret,
    };
    GenericSig {
        formals: header
            .type_parameters
            .iter()
            .filter_map(|source| type_params.bound(source).ty_param_name())
            .map(str::to_string)
            .collect(),
        formal_bounds,
        receiver,
        params,
        ret,
        return_policy: Default::default(),
    }
}

pub(super) fn source_generic_signature_from_header(
    header: &StreamedCallableHeader,
    classes: &ClassNames,
    type_params: &TParams,
    resolved_ret: Ty,
    inferred_ret_tparam: Option<String>,
    diags: &mut DiagSink,
) -> GenericSig {
    let resolve =
        |reference, diags: &mut DiagSink| ty_of_ref(reference, classes, type_params, diags);
    let receiver = header
        .receiver
        .as_ref()
        .map(|reference| resolve(reference, diags));
    let params = header
        .parameters
        .iter()
        .map(|parameter| {
            let ty = resolve(&parameter.ty, diags);
            semantic_value_parameter_ty(ty, parameter.is_vararg)
        })
        .collect();
    let ret = header
        .explicit_result
        .as_ref()
        .map(|reference| resolve(reference, diags))
        .unwrap_or_else(|| {
            let bindings = header
                .type_parameters
                .iter()
                .map(|source| (source.clone(), type_params.bound(source)))
                .collect::<HashMap<_, _>>();
            crate::symbol_resolver::ty_subst_keep_unbound(resolved_ret, &bindings)
        });
    // An inferred return that is one of the declared type parameters. `resolved_ret` is the erased
    // inference (`fun <T> foo(x: T) = x` infers `Any`), and recording that erasure in `@Metadata`
    // says the function returns `Any`, leaving kotlin-reflect unable to resolve the declaration
    // against its bytecode method.
    let ret = match (header.result, inferred_ret_tparam) {
        (StreamedResultKind::Inferred, Some(name)) => type_params.bound(&name),
        _ => ret,
    };
    let formal_bounds = header
        .type_parameters
        .iter()
        .map(|parameter| {
            header
                .bounds
                .iter()
                .filter(|(owner, _)| owner == parameter)
                .map(|(_, bound)| resolve(bound, diags))
                .collect()
        })
        .collect();
    GenericSig {
        formals: header
            .type_parameters
            .iter()
            .filter_map(|source| type_params.bound(source).ty_param_name())
            .map(str::to_string)
            .collect(),
        formal_bounds,
        receiver,
        params,
        ret,
        return_policy: Default::default(),
    }
}
