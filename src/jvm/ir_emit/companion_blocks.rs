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
use crate::metadata::class_builder::{JvmFieldSignature, JvmPropertySignature};

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

/// How the JVM realizes block property `property` (`@Metadata`'s property signature): its static
/// storage behind public static accessors, exactly as a top-level property's on its facade.
pub(super) fn property_signature(
    ir: &IrFile,
    property: &crate::ir::IrCompanionBlockProperty,
) -> JvmPropertySignature {
    let local_classifiers = crate::metadata::local_classifiers::names(ir);
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
    JvmPropertySignature {
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
        field: storage.map(|storage| JvmFieldSignature {
            name: None,
            desc: Some(type_descriptor(storage.ty)).filter(|physical| {
                super::super::metadata_method_signatures::requires_field_signature(
                    property.ty,
                    physical,
                    &local_classifiers,
                )
            }),
        }),
        synthetic_method: None,
        moved_from_interface_companion: false,
    }
}
