//! Provider normalization for JVM-backed top-level Kotlin properties.
//!
//! A Kotlin `const val` is declared in package metadata but may have no emitted getter. Its field is
//! the physical realization and carries the compile-time payload. This module joins those two
//! provider facts into the same [`PropertyInfo`] shape used by source and accessor-backed properties.

use super::classpath::kotlin_type_name_to_ty;
use super::jvm_libraries::JvmStaticField;
use super::metadata::MetaProp;
use crate::libraries::{LibraryCallable, PropertyInfo};
use crate::types::Ty;

pub(super) fn merge_top_level_const(
    source_name: &str,
    metadata: &MetaProp,
    field: &JvmStaticField,
    properties: &mut Vec<PropertyInfo>,
) {
    if metadata.name != source_name || !metadata.is_const || metadata.is_extension {
        return;
    }
    let Some(constant) = field.constant.clone() else {
        return;
    };

    if let Some(property) = properties.iter_mut().find(|property| {
        property.kind == crate::libraries::PropKind::TopLevel
            && property.owner == field.owner
            && property.name == source_name
    }) {
        property.is_const = true;
        property.compile_time_constant = Some(constant);
        return;
    }

    let ty = metadata.generic_sig.as_ref().map_or_else(
        || {
            let declared = metadata.ret_class.map_or(field.ty, kotlin_type_name_to_ty);
            if metadata.ret_nullable {
                Ty::nullable(declared)
            } else {
                declared
            }
        },
        |signature| signature.ret,
    );
    // There is no JVM getter method. Model the semantic zero-argument property read with the exact
    // provider-owned static-field identity, so any consumer that needs a runtime handle cannot invent
    // an accessor name or descriptor. Ordinary value reads consume `compile_time_constant` first.
    let mut getter = LibraryCallable::library(
        field.owner,
        source_name,
        Vec::new(),
        ty,
        field.ty,
        field.descriptor.clone(),
    );
    getter.external_identity = field.external_identity;
    properties.push(PropertyInfo {
        name: source_name.to_string(),
        kind: crate::libraries::PropKind::TopLevel,
        receiver: None,
        formals: metadata
            .generic_sig
            .as_ref()
            .map(|signature| signature.formals.clone())
            .unwrap_or_default(),
        ty,
        context_count: metadata.context_params.len(),
        context_param_names: metadata
            .context_params
            .iter()
            .map(|parameter| parameter.name.clone())
            .collect(),
        getter,
        setter: None,
        setter_visibility: metadata.visibility,
        is_const: true,
        compile_time_constant: Some(constant),
        visibility: metadata.visibility,
        owner: field.owner,
        receiver_rank: 0,
        source_key: None,
        stable_declaration: None,
        getter_declaration: None,
        setter_declaration: None,
        source_member: None,
        accessor_derived: false,
    });
}
