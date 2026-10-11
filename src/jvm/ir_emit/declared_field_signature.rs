//! The generic `Signature` attribute of a class's declared field, shared by the ordinary class,
//! named-object and interface-companion field tables.

use super::signature_formatter::JvmSignatureFormatter;
use super::{property_jvm_signatures, IrClass, IrFile};

/// `c.fields[field_index]`'s `Signature`: `T<P>;` for a field typed by a bare type parameter, the
/// full generic form for a parameterized type, and none for a delegated property's `x$delegate`
/// storage, for which kotlinc's `ClassCodegen.generateField` writes none (`PROPERTY_DELEGATE`)
/// whatever the delegate's type arguments.
pub(super) fn declared_field_signature(
    ir: &IrFile,
    formatter: &JvmSignatureFormatter<'_>,
    c: &IrClass,
    fq_name: &str,
    field_index: usize,
) -> Option<String> {
    if stores_property_delegate(ir, c, field_index) {
        return None;
    }
    let field = &c.fields[field_index];
    let type_parameter = ir
        .field_signatures(fq_name)
        .and_then(|signatures| signatures.iter().find(|(name, _)| *name == field.name))
        .map(|(_, parameter)| parameter.as_str())
        .or(field.type_param.as_deref());
    property_jvm_signatures(formatter, &field.ty, type_parameter).field
}

/// Whether `c.fields[field_index]` holds a declared or member-extension property's delegate.
fn stores_property_delegate(ir: &IrFile, c: &IrClass, field_index: usize) -> bool {
    let index = Some(field_index as u32);
    c.properties
        .iter()
        .any(|property| property.delegate_field == index)
        || ir
            .member_ext_props
            .get(&c.fq_name_id())
            .is_some_and(|extensions| extensions.iter().any(|ext| ext.delegate_field == index))
}
