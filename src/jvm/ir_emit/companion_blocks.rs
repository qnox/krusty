//! JVM realization of `companion { … }` block members, which are static members of the class
//! that declares the block.
//!
//! A block function is a `public static final` method of the class; a block property is a static
//! field of the class behind `public static` accessors, laid out exactly like a top-level property
//! on its facade. Both are recorded in the class's `@Metadata` with the companion flag. Field
//! storage and initialization are [`static_fields`]'s; this module owns only what distinguishes a
//! block member.

use super::*;
use crate::jvm::names::type_descriptor;
use crate::jvm::parameter_names;
use crate::metadata::class_builder::PropMeta;

/// Whether static `index` is block-property storage, whose field is private behind its accessors
/// exactly as a top-level property's is on the facade; only a `const` keeps its declared
/// visibility.
pub(super) fn storage_is_private(ir: &IrFile, index: u32) -> bool {
    ir.companion_blocks.is_storage(index) && !ir.statics[index as usize].is_const
}

/// Whether other classes read block property `index` through its class's compiler-default
/// public `getX` (see `SourceOrderedMember::StaticDefaultAccessor`).
pub(super) fn getter_owned(ir: &IrFile, index: u32) -> bool {
    ir.companion_blocks.is_storage(index) && ir.has_jvm_default_static_getter(index)
}

/// Whether other classes write block property `index` through its class's compiler-default
/// public `setX`; each accessor is decided on its own.
pub(super) fn setter_owned(ir: &IrFile, index: u32) -> bool {
    ir.companion_blocks.is_storage(index) && ir.has_jvm_default_static_setter(index)
}

/// Record class `c`'s block properties among its declared `@Metadata` properties.
pub(super) fn push_property_metadata(
    ir: &IrFile,
    c: &IrClass,
    declared_props: &mut Vec<(u32, PropMeta)>,
) {
    let local_classifiers = super::super::local_classifiers::names(ir);
    for property in ir.companion_blocks.properties_of(c.fq_name_id()) {
        let accessor_sig = |fid: u32| {
            ir.functions.get(fid as usize).map(|function| {
                (
                    function.name.clone(),
                    ir_method_desc(&function.params, &function.ret),
                )
            })
        };
        let storage = property
            .storage
            .map(|storage| &ir.statics[storage as usize]);
        let default_getter = || {
            (
                property_getter_name(&property.name),
                format!("(){}", type_descriptor(property.ty)),
            )
        };
        let default_setter = || {
            (
                storage
                    .and_then(|storage| storage.setter_jvm_name.clone())
                    .unwrap_or_else(|| property_setter_name(&property.name)),
                format!("({})V", type_descriptor(property.ty)),
            )
        };
        declared_props.push((
            property.source_order,
            PropMeta {
                return_value_status: Default::default(),
                spellings: crate::spelling::DeclaredSpellings::default(),
                name: property.name.clone(),
                ty: property.ty,
                context_params: Vec::new(),
                is_var: property.is_var,
                visibility: property.visibility,
                has_constant: property.has_constant,
                is_const: property.is_const,
                modifiers: property.modifiers,
                setter_visibility: property.setter_visibility,
                has_backing_field: storage.is_some(),
                tparam: None,
                receiver: None,
                type_params: Vec::new(),
                getter: (!property.is_const && !property.visibility.is_private())
                    .then(|| {
                        property
                            .getter
                            .map_or_else(|| Some(default_getter()), accessor_sig)
                    })
                    .flatten(),
                setter: (property.is_var && !property.visibility.is_private())
                    .then(|| {
                        property
                            .setter
                            .map_or_else(|| Some(default_setter()), accessor_sig)
                    })
                    .flatten(),
                setter_parameter_name: parameter_names::explicit_setter(ir, property.setter),
                field_desc: storage
                    .map(|storage| type_descriptor(storage.ty))
                    .filter(|physical| {
                        super::super::metadata_method_signatures::requires_field_signature(
                            property.ty,
                            physical,
                            &local_classifiers,
                        )
                    }),
                field_name: None,
                annotations: Default::default(),
                field_annotations: Default::default(),
                accessor_annotations: Default::default(),
                synthetic_method: None,
                moved_from_interface_companion: false,
                companion: true,
            },
        ));
    }
}
