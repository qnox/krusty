//! Kotlin declaration records built from common IR, before any target realization.
//!
//! Package metadata describes declarations as Kotlin sees them: names, declared types, modifiers and
//! parameter roles. Every carrier writes that same record, so it is built here once from the
//! checked IR declaration tables. A target adds only what it realizes the declaration with — the
//! JVM its method and field signatures — on top.

use crate::ir::{IrFile, IrParameterIdentity, IrParameterRole};

use super::builder::{FnMeta, PropMeta, TypeAliasMeta};

/// A package function's record, without any target signature.
pub(crate) fn package_function(ir: &IrFile, declaration: &crate::ir::IrPackageFunction) -> FnMeta {
    let mut param_annotations = ir
        .fn_param_annotations
        .get(&declaration.function)
        .cloned()
        .unwrap_or_default();
    if declaration.receiver.is_some() && declaration.context_count < param_annotations.len() {
        param_annotations.remove(declaration.context_count);
    }
    let mut no_infer_params = ir
        .fn_param_no_infer
        .get(&declaration.function)
        .cloned()
        .unwrap_or_default();
    if declaration.receiver.is_some() && declaration.context_count < no_infer_params.len() {
        no_infer_params.remove(declaration.context_count);
    }
    let context_parameter_kinds = ir
        .function_parameter_identities(declaration.function)
        .expect("a package function metadata declaration retains parameter identities")
        .iter()
        .take(declaration.context_count)
        .map(context_kind)
        .collect();
    FnMeta {
        name: declaration.name.clone(),
        params: declaration.params.clone(),
        ret: declaration.ret,
        decl_order: declaration.source_order as usize,
        annotations: crate::metadata::MetadataAnnotations::of_optional(
            ir.function_annotations.get(&declaration.function),
        ),
        receiver: declaration.receiver,
        param_modifiers: declared_value_parameters(
            ir,
            declaration.function,
            declaration.param_defaults.iter().copied(),
        ),
        suspend: declaration.suspend,
        jvm_desc: None,
        jvm_name: None,
        inline: declaration.inline,
        has_function_typed_parameter: declaration.has_function_typed_parameter,
        operator: declaration.operator,
        infix: declaration.infix,
        tailrec: declaration.tailrec,
        companion: declaration.companion,
        contract: declaration
            .contract
            .as_ref()
            .map(|contract| contract.to_arc()),
        type_params: declaration
            .type_params
            .iter()
            .map(|parameter| (parameter.name.clone(), parameter.reified))
            .collect(),
        semantic_type_params: declaration
            .type_params
            .iter()
            .map(|parameter| parameter.semantic_name.clone())
            .collect(),
        type_param_bounds: declaration
            .type_params
            .iter()
            .map(|parameter| parameter.bounds.clone())
            .collect(),
        context_count: declaration.context_count,
        context_parameter_kinds,
        vararg_index: declaration.vararg_index,
        visibility: declaration.visibility,
        spellings: declaration.spellings.clone(),
        param_annotations: param_annotations
            .iter()
            .map(crate::metadata::MetadataAnnotations::of)
            .collect(),
        no_infer_params,
    }
}

/// A package property's record, without any target accessor or field signature. `setter` is the
/// common-IR function of a source-declared setter, whose parameter name the record keeps.
pub(crate) fn package_property(
    ir: &IrFile,
    declaration: &crate::ir::IrPackageProperty,
    setter: Option<u32>,
) -> PropMeta {
    PropMeta {
        name: declaration.name.clone(),
        ty: declaration.ty,
        is_var: declaration.mutable,
        type_params: declaration
            .type_params
            .iter()
            .map(|parameter| parameter.name.clone())
            .collect(),
        semantic_type_params: declaration
            .type_params
            .iter()
            .map(|parameter| parameter.semantic_name.clone())
            .collect(),
        type_param_bounds: declaration
            .type_params
            .iter()
            .map(|parameter| parameter.bounds.clone())
            .collect(),
        receiver: declaration.receiver,
        context_params: declaration
            .context_parameter_names
            .iter()
            .cloned()
            .zip(declaration.context_parameter_kinds.iter().copied())
            .zip(declaration.context_parameters.iter().copied())
            .map(|((name, kind), ty)| (name, kind, ty))
            .collect(),
        getter: None,
        setter: None,
        setter_parameter_name: explicit_setter_name(ir, setter),
        is_const: declaration.is_const,
        has_constant: declaration.has_constant,
        decl_order: declaration.source_order as usize,
        visibility: declaration.visibility,
        spellings: declaration.spellings.clone(),
        has_backing_field: declaration.has_backing_field,
        modifiers: declaration.modifiers,
        setter_visibility: declaration.setter_visibility,
        companion: declaration.is_companion_extension(),
        accessor_annotations: ir
            .accessor_annotations
            .get(&declaration.property)
            .map(crate::metadata::AccessorMetadataAnnotations::of)
            .unwrap_or_default(),
        field_name: None,
        field_desc: None,
    }
}

/// A package typealias's record.
pub(crate) fn package_alias(alias: &crate::ir::IrTypeAlias) -> TypeAliasMeta {
    TypeAliasMeta {
        name: alias.name.clone(),
        formals: alias.formals.clone(),
        expansion: alias.expansion,
        visibility: alias.visibility,
        expansion_spelling: alias.expansion_spelling.clone(),
        decl_order: alias.source_order as usize,
    }
}

/// The name source wrote for a setter's value parameter, which Kotlin metadata records. The final
/// semantic parameter is the setter value by the accessor contract; its typed IR identity
/// distinguishes a written name from the compiler-generated implicit setter value.
pub(crate) fn explicit_setter_name(ir: &IrFile, setter: Option<u32>) -> Option<String> {
    let identity = ir.function_parameter_identities(setter?)?.last()?;
    (identity.role == IrParameterRole::Value
        && identity.provenance == crate::ir::IrParameterProvenance::SourceDeclared)
        .then(|| parameter_name(identity).map(str::to_owned))
        .flatten()
}

/// Kotlin metadata accepts only a declaration/producer-published semantic name. A lambda's `_`
/// parameter, which declares no name, is kotlinc's `<unused var>` like an anonymous context
/// parameter; a destructuring one is `<destruct>`.
pub(crate) fn parameter_name(identity: &IrParameterIdentity) -> Option<&str> {
    match identity.role {
        IrParameterRole::AnonymousContextParameter { .. } | IrParameterRole::UnusedValue => {
            Some("<unused var>")
        }
        IrParameterRole::DestructuredValue => Some(DESTRUCTURED),
        _ => identity.source_name.as_deref(),
    }
}

/// Kotlin's special name for a parameter written as a destructuring declaration. A suspend lambda's
/// body also reads the parameter back into a local of this name.
pub(crate) const DESTRUCTURED: &str = "<destruct>";

pub(crate) fn context_kind(identity: &IrParameterIdentity) -> crate::types::ContextParameterKind {
    match identity.role {
        IrParameterRole::ContextValue => crate::types::ContextParameterKind::Named,
        IrParameterRole::AnonymousContextParameter { .. } => {
            crate::types::ContextParameterKind::Anonymous
        }
        IrParameterRole::ContextReceiver { .. } => {
            crate::types::ContextParameterKind::LegacyReceiver
        }
        _ => panic!("a metadata context prefix must retain its semantic role"),
    }
}

/// What each value parameter of `fid` declared, extension receiver excluded: `defaults` says which
/// write a default value, and the IR's recorded inline modifiers supply `crossinline`/`noinline`.
pub(crate) fn declared_value_parameters(
    ir: &IrFile,
    fid: u32,
    defaults: impl IntoIterator<Item = bool>,
) -> Vec<crate::metadata::DeclaredValueParameter> {
    let modifiers = ir.declared_inline_modifiers(fid);
    let mut declared = defaults
        .into_iter()
        .map(crate::metadata::DeclaredValueParameter::defaulted)
        .collect::<Vec<_>>();
    if declared.len() < modifiers.len() {
        declared.resize(modifiers.len(), Default::default());
    }
    for (parameter, modifier) in declared.iter_mut().zip(modifiers) {
        parameter.inline_modifier = modifier;
    }
    declared
}
