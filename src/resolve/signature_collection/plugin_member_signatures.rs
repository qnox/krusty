//! The signature of a member a compiler plugin's frontend hook declares on a source class.
//!
//! A plugin publishes only the facts its declaration has: parameters with their names, result,
//! generic signature, whether it overrides a supertype member, and the plugin operation that
//! implements it. Every other signature fact is the one of a final public member with no source
//! declaration behind it.

use crate::plugins::{FrontendCallable, FrontendCallableOwner};

use super::*;

/// `member`'s name, where it is declared, and its signature.
pub(super) fn frontend_plugin_member_signature(
    member: FrontendCallable,
) -> (String, FrontendCallableOwner, Signature) {
    let required = member.params.len();
    assert_eq!(
        member.param_names.len(),
        required,
        "a frontend plugin declaration publishes one identity per parameter"
    );
    let signature = Signature {
        params: member.params,
        ret: member.ret,
        generic_sig: member.generic_sig,
        projected_return_hazard: false,
        flags: SigFlags::default()
            .with_is_final(true)
            .with_is_override(member.is_override),
        annotations: Vec::new(),
        equality_bound: None,
        vararg_index: None,
        required,
        param_defaults: vec![].into(),
        exact_params: vec![].into(),
        no_infer_params: vec![].into(),
        implicit_integer_coercion: vec![].into(),
        param_default_values: Vec::new(),
        param_names: member.param_names,
        parameter_identities: Vec::new(),
        lambda_param_types: vec![],
        lambda_recv: vec![].into(),
        inline_modifiers: Vec::new(),
        visibility: Visibility::Public,
        context_count: 0,
        source_decl: None,
        stable_declaration: None,
        source_file: None,
        source_member: None,
        source_receiver: None,
        package: TypeName::ROOT,
        contract: None,
        plugin_expression: member.plugin_expression,
    };
    (member.name, member.owner, signature)
}
