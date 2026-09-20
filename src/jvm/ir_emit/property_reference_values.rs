//! Value conversion at the erased `KProperty` boundary.

use super::{
    box_prim_free, ir_ty_to_jvm, semantic_scalar_adapter, slot_words, type_descriptor,
    verif_for_jvm_free,
};
use crate::jvm::classfile::{ClassWriter, CodeBuilder, VerifType};
use crate::types::{Ty, TypeName};

/// Convert a value class at the erased `KProperty` boundary, letting `null` pass through.
///
/// A nullable value class crosses that boundary as either its box or `null`. An unconditional
/// `box-impl` dereferences the null instead, so kotlinc guards the conversion on exactly that case.
pub(super) fn value_class_boundary_conversion(
    cw: &mut ClassWriter,
    code: &mut CodeBuilder,
    nullable: bool,
    locals: Vec<VerifType>,
    boxed: VerifType,
    result: Ty,
    convert: impl FnOnce(&mut ClassWriter, &mut CodeBuilder),
) {
    if !nullable {
        convert(cw, code);
        return;
    }
    let null_case = code.new_label();
    let done = code.new_label();
    code.dup();
    // The duplicate remains on the stack at the null branch: preserve the value as it arrived.
    code.add_frame_if_new(null_case, locals.clone(), vec![boxed]);
    code.ifnull(null_case);
    convert(cw, code);
    let converted = verif_for_jvm_free(cw, result);
    code.add_frame_if_new(done, locals, vec![converted]);
    code.goto(done);
    code.bind(null_case);
    code.pop();
    code.aconst_null();
    code.bind(done);
}

pub(super) fn box_property_reference_value(
    cw: &mut ClassWriter,
    code: &mut CodeBuilder,
    property: &crate::ir::PropRef,
    boxed_value_class: Option<TypeName>,
    physical: Ty,
    locals: Vec<VerifType>,
) {
    if let Some(value_class) = boxed_value_class {
        let owner = value_class.render();
        let descriptor = format!("({})L{owner};", type_descriptor(ir_ty_to_jvm(&physical)));
        let method = cw.methodref(&owner, "box-impl", &descriptor);
        let carrier = verif_for_jvm_free(cw, ir_ty_to_jvm(&physical));
        value_class_boundary_conversion(
            cw,
            code,
            property.prop_ty.is_nullable(),
            locals,
            carrier,
            Ty::obj_name(value_class),
            |_, code| code.invokestatic(method, slot_words(ir_ty_to_jvm(&physical)) as i32, 1),
        );
    } else if physical.is_jvm_scalar() {
        box_prim_free(
            cw,
            code,
            semantic_scalar_adapter(property.prop_ty, physical),
        );
    }
}
