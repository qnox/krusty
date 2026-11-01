//! Provider normalization for JVM-backed top-level Kotlin properties.
//!
//! A Kotlin `const val` or `@JvmField` property is declared in package metadata (or in class
//! metadata, for a `companion { … }` block member) but has no emitted accessor. Its static field is
//! the physical realization, and for a constant it carries the compile-time payload. This module
//! joins those two provider facts into the same [`PropertyInfo`] shape used by source and
//! accessor-backed properties.

use super::classpath::kotlin_type_name_to_ty;
use super::jvm_libraries::JvmStaticField;
use super::metadata::MetaProp;
use crate::libraries::{LibraryCallable, PropertyInfo};
use crate::types::Ty;

pub(super) fn merge_metadata_const(
    source_name: &str,
    metadata: &MetaProp,
    field: &JvmStaticField,
    deprecated_hidden: bool,
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
        property.deprecated_hidden = deprecated_hidden;
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
    let getter_source_name = source_name.to_string();
    let getter_physical_name = (field.name != source_name).then(|| field.name.clone());
    let mut getter = LibraryCallable::library(
        field.owner,
        getter_source_name,
        Vec::new(),
        ty,
        field.ty,
        field.descriptor.clone(),
    );
    getter.physical_name = getter_physical_name;
    getter.external_identity = field.external_identity;
    properties.push(field_property(
        source_name,
        metadata,
        field,
        ty,
        FieldAccessors {
            getter,
            setter: None,
        },
        Some(constant),
        deprecated_hidden,
    ));
}

/// The read, and for a `var` the write, of the static field that realizes a property.
pub(super) struct FieldAccessors {
    pub(super) getter: LibraryCallable,
    pub(super) setter: Option<LibraryCallable>,
}

/// Joins a receiver-less metadata property that has no accessor at all, a `@JvmField` one, with the
/// static field realizing it: its value is read and written through `accessors`, and its repeated
/// reads have `read_stability` in this compilation. An accessor-backed declaration of the same
/// property, already among `properties`, is that property's realization.
pub(super) fn merge_metadata_field(
    source_name: &str,
    metadata: &MetaProp,
    field: &JvmStaticField,
    accessors: FieldAccessors,
    read_stability: crate::libraries::PropertyReadStability,
    deprecated_hidden: bool,
    properties: &mut Vec<PropertyInfo>,
) {
    if metadata.name != source_name
        || metadata.is_const
        || metadata.is_extension
        || metadata.getter.is_some()
    {
        return;
    }
    if properties.iter().any(|property| {
        property.kind == crate::libraries::PropKind::TopLevel
            && property.owner == field.owner
            && property.name == source_name
    }) {
        return;
    }
    let accessors = FieldAccessors {
        setter: accessors.setter.filter(|_| metadata.is_var),
        ..accessors
    };
    let mut property = field_property(
        source_name,
        metadata,
        field,
        field.ty,
        accessors,
        None,
        deprecated_hidden,
    );
    property.read_stability = read_stability;
    properties.push(property);
}

/// The [`PropertyInfo`] of a receiver-less property `metadata` declares and `field` realizes.
fn field_property(
    source_name: &str,
    metadata: &MetaProp,
    field: &JvmStaticField,
    ty: Ty,
    accessors: FieldAccessors,
    constant: Option<crate::libraries::LibraryConst>,
    deprecated_hidden: bool,
) -> PropertyInfo {
    let is_const = constant.is_some();
    PropertyInfo {
        return_value_status: None,
        name: source_name.to_string(),
        kind: crate::libraries::PropKind::TopLevel,
        receiver: None,
        associated_classifier: None,
        associated_access_owner: None,
        companion_associated: false,
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
        context_parameter_identities: metadata.context_parameter_identities(),
        getter: accessors.getter,
        setter: accessors.setter.map(Box::new),
        setter_visibility: metadata.visibility,
        setter_parameter_name: None,
        is_const,
        implicit_integer_coercion: false,
        compile_time_constant: constant,
        metadata_constant_read: false,
        deprecated_hidden,
        accessor_availability: Default::default(),
        is_final: true,
        visibility: metadata.visibility,
        owner: field.owner,
        receiver_rank: 0,
        source_key: None,
        stable_declaration: None,
        source_visible: true,
        getter_declaration: None,
        setter_declaration: None,
        source_member: None,
        producer: crate::libraries::PropertyProducer::Field,
        read_stability: crate::libraries::PropertyReadStability::Unstable,
    }
}
