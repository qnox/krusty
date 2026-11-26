//! The JVM descriptors of a class's primary constructor and of the fields its constructor
//! parameters store.

use super::capture_order;
use super::declaration_types::class_ctor_jvm_tys;
use krusty::ir::{IrClass, IrFile};

/// The primary constructor's JVM descriptor: every constructor argument, property-backed or plain.
pub(super) fn primary_ctor_descriptor(ir: &IrFile, c: &IrClass) -> String {
    format!("({})V", primary_ctor_parameter_descs(ir, c))
}

pub(super) fn primary_ctor_parameter_descs(ir: &IrFile, c: &IrClass) -> String {
    class_ctor_jvm_tys(ir, c)
        .into_iter()
        .map(krusty::jvm::names::type_descriptor)
        .collect()
}

pub(super) fn ctor_field_descs(ir: &IrFile, c: &IrClass) -> String {
    let fields = c.fields.iter().take(c.ctor_param_count as usize).collect();
    capture_order::physical_fields(ir, c, fields)
        .into_iter()
        .map(|f| krusty::jvm::names::type_descriptor(f.ty))
        .collect()
}
