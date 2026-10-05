//! Parameter identities carried by a source [`Signature`](super::Signature).
//!
//! The identities are captured when the signature is built and copied onto the call signature.
//! Later phases read that record; they do not rebuild a parameter from its spelling.

use crate::ast::{DeclId, Param};
use crate::fir::ResolvedParameterIdentity;
use crate::libraries::{CallSig, FunctionInfo};
use crate::types::TypeName;

use super::{SigFlags, Signature};

/// Copy source identities onto `call_sig` when the signature recorded a full parallel list.
pub(super) fn copy_parameter_identities(
    call_sig: &mut CallSig,
    identities: &[ResolvedParameterIdentity],
    param_count: usize,
) {
    if identities.is_empty() {
        return;
    }
    assert_eq!(
        identities.len(),
        param_count,
        "a source signature's parameter identities must cover its logical parameters"
    );
    call_sig.parameter_identities = identities.to_vec();
}

/// Identities for a local function's declared parameters, in source order.
pub(super) fn declared_parameter_identities(params: &[Param]) -> Vec<ResolvedParameterIdentity> {
    params
        .iter()
        .enumerate()
        .map(|(ordinal, parameter)| {
            ResolvedParameterIdentity::declared(
                ordinal as u32,
                &parameter.name,
                parameter.context_kind,
            )
        })
        .collect()
}

/// Keep provider identities only when they line up with the semantic parameter list.
pub(super) fn aligned_parameter_identities(
    call_sig: &CallSig,
    aligned: bool,
) -> Vec<ResolvedParameterIdentity> {
    if aligned {
        call_sig.parameter_identities.clone()
    } else {
        Vec::new()
    }
}

pub(super) fn signature_from_resolved_function(function: &FunctionInfo) -> Signature {
    let params = function.semantic_params().to_vec();
    let (source_file, source_decl) = function
        .source_key
        .map(|(file, declaration)| (Some(file), Some(DeclId(declaration))))
        .unwrap_or((None, None));
    Signature {
        params: params.clone(),
        ret: function.ret.apply(function.callable.ret),
        generic_sig: function.generic_sig.clone(),
        projected_return_hazard: function.projected_return_hazard,
        flags: SigFlags::default()
            .with_vararg(function.call_sig.vararg_index.is_some())
            .with_is_inline(function.flags.inline.can_inline())
            .with_is_operator(function.flags.operator)
            .with_is_infix(function.flags.infix)
            .with_is_suspend(function.flags.suspend)
            .with_has_reified_type_params(function.flags.reified)
            .with_requires_splice(function.flags.inline.must_inline()),
        annotations: function.callable.annotations.clone(),
        equality_bound: function.callable.equality_bound,
        vararg_index: function.call_sig.vararg_index,
        required: function.call_sig.required,
        param_defaults: function.call_sig.param_defaults.clone(),
        exact_params: function.call_sig.exact_params.clone(),
        no_infer_params: function.call_sig.no_infer_params.clone(),
        implicit_integer_coercion: function.call_sig.implicit_integer_coercion.clone(),
        param_default_values: Vec::new(),
        param_names: function.call_sig.param_names.clone(),
        parameter_identities: function.call_sig.parameter_identities.clone(),
        lambda_param_types: function.call_sig.lambda_param_types.clone(),
        lambda_recv: function.call_sig.lambda_receiver_params.clone(),
        inline_modifiers: function.call_sig.inline_modifiers.clone(),
        visibility: function.visibility,
        context_count: function.context_count,
        source_decl,
        stable_declaration: function.stable_declaration,
        source_file,
        source_member: None,
        source_receiver: function.receiver,
        package: TypeName::ROOT,
        contract: None,
        plugin_expression: function.callable.plugin_expression,
    }
}
