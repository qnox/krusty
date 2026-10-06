//! Kotlin metadata for a value class's `equals`/`hashCode`/`toString` overrides.

use crate::ir::IrValueClassAnyMember;
use crate::metadata::class_builder::{FnMeta, EQUALS_FN_FLAGS, HASHCODE_TOSTRING_FN_FLAGS};
use crate::types::Ty;

/// The overrides a value class's synthesis generated, in kotlinc's order. Each dispatches to a
/// differently-named static `-impl` taking the erased `underlying` descriptor, so each records a
/// `JvmMethodSignature` (name + desc). A source-declared override is a declared function instead.
pub(super) fn functions(underlying: &str, generated: &[IrValueClassAnyMember]) -> Vec<FnMeta> {
    let mut generated = generated.to_vec();
    generated.sort_unstable();
    generated
        .into_iter()
        .map(|member| match member {
            IrValueClassAnyMember::Equals => equals(underlying),
            IrValueClassAnyMember::HashCode => hash_code(underlying),
            IrValueClassAnyMember::ToString => to_string(underlying),
        })
        .collect()
}

fn equals(underlying: &str) -> FnMeta {
    FnMeta {
        context_count: 0,
        context_parameter_kinds: Vec::new(),
        spellings: crate::spelling::DeclaredSpellings::default(),
        name: "equals".into(),
        params: vec![("other".into(), Ty::nullable(Ty::obj("kotlin/Any")))],
        ret: Ty::Boolean,
        type_params: Vec::new(),
        semantic_type_params: Vec::new(),
        type_param_bounds: Vec::new(),
        flags: EQUALS_FN_FLAGS,
        has_function_typed_parameter: false,
        params_have_defaults: false,
        receiver: None,
        param_modifiers: Vec::new(),
        vararg_index: None,
        jvm_sig: Some(format!("({underlying}Ljava/lang/Object;)Z")),
        jvm_sig_name: Some("equals-impl".into()),
        annotations: Default::default(),
        param_annotations: Vec::new(),
        no_infer_params: Vec::new(),
        contract: None,
    }
}

fn hash_code(underlying: &str) -> FnMeta {
    FnMeta {
        context_count: 0,
        context_parameter_kinds: Vec::new(),
        spellings: crate::spelling::DeclaredSpellings::default(),
        name: "hashCode".into(),
        params: vec![],
        ret: Ty::Int,
        type_params: Vec::new(),
        semantic_type_params: Vec::new(),
        type_param_bounds: Vec::new(),
        flags: HASHCODE_TOSTRING_FN_FLAGS,
        has_function_typed_parameter: false,
        params_have_defaults: false,
        receiver: None,
        param_modifiers: Vec::new(),
        vararg_index: None,
        jvm_sig: Some(format!("({underlying})I")),
        jvm_sig_name: Some("hashCode-impl".into()),
        annotations: Default::default(),
        param_annotations: Vec::new(),
        no_infer_params: Vec::new(),
        contract: None,
    }
}

fn to_string(underlying: &str) -> FnMeta {
    FnMeta {
        context_count: 0,
        context_parameter_kinds: Vec::new(),
        spellings: crate::spelling::DeclaredSpellings::default(),
        name: "toString".into(),
        params: vec![],
        ret: Ty::String,
        type_params: Vec::new(),
        semantic_type_params: Vec::new(),
        type_param_bounds: Vec::new(),
        flags: HASHCODE_TOSTRING_FN_FLAGS,
        has_function_typed_parameter: false,
        params_have_defaults: false,
        receiver: None,
        param_modifiers: Vec::new(),
        vararg_index: None,
        jvm_sig: Some(format!("({underlying})Ljava/lang/String;")),
        jvm_sig_name: Some("toString-impl".into()),
        annotations: Default::default(),
        param_annotations: Vec::new(),
        no_infer_params: Vec::new(),
        contract: None,
    }
}
