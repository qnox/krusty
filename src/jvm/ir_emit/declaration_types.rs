//! Physical JVM types and descriptors for common-IR declarations.
//!
//! Declaration descriptors themselves live in `jvm::method_descriptors`, which planning and
//! emission both call. This module keeps the function- and field-shaped adapters that need the
//! file's capture map.

use super::*;

pub(crate) use crate::jvm::method_descriptors::jvm_tys;
pub(in crate::jvm) use crate::jvm::method_descriptors::{ir_method_desc, jvm_declared_ty};

pub(super) fn jvm_function_params(ir: &IrFile, function: crate::ir::FunId) -> Vec<Ty> {
    let mut parameters = jvm_tys(&ir.functions[function as usize].params);
    for (parameter, physical) in parameters.iter_mut().enumerate() {
        let ordinal = u32::try_from(parameter).expect("too many JVM function parameters");
        if ir
            .shared_capture_parameters
            .contains_key(&(function, ordinal))
        {
            *physical = Ty::obj(ref_class(physical).0);
        }
    }
    parameters
}

/// The types a common-IR function's generic `Signature` spells: its declared parameters, with each
/// shared mutable capture typed as its cell. kotlinc types an object cell `Ref.ObjectRef<T>`, so a
/// lifted function that takes one signs `(Lkotlin/jvm/internal/Ref$ObjectRef<Ljava/lang/String;>;)V`.
pub(super) fn signature_function_params(ir: &IrFile, function: crate::ir::FunId) -> Vec<Ty> {
    let declared = &ir.functions[function as usize].params;
    (0..declared.len())
        .map(|parameter| {
            let ordinal = u32::try_from(parameter).expect("too many JVM function parameters");
            ir.shared_capture_parameters
                .get(&(function, ordinal))
                .map_or(declared[parameter], crate::jvm::shared_captures::holder_ty)
        })
        .collect()
}

/// The JVM descriptor a common-IR function is declared with: its result is the wrapper where
/// `override_results` boxes it.
pub(in crate::jvm) fn function_descriptor(
    ir: &IrFile,
    override_results: &crate::jvm::override_results::OverrideResults,
    function: crate::ir::FunId,
) -> String {
    method_descriptor(
        &jvm_function_params(ir, function),
        jvm_declared_ty(&override_results.physical_result(ir, function)),
    )
}

pub(super) fn jvm_is_erased_top(ty: Ty) -> bool {
    match ty.obj_internal() {
        Some(name) if crate::jvm::jvm_class_map::is_jvm_erased_top(name) => true,
        _ => ty.array_elem().is_some_and(jvm_is_erased_top),
    }
}

pub(super) fn ir_type_desc(ty: &Ty) -> String {
    type_descriptor(jvm_declared_ty(ty))
}

pub(super) fn local_variable_desc(ty: Ty) -> String {
    type_descriptor(if ty == Ty::Unit {
        Ty::obj("kotlin/Unit")
    } else {
        ty
    })
}

pub(super) fn field_jvm_tys(fields: &[IrField]) -> Vec<Ty> {
    fields
        .iter()
        .map(|field| jvm_declared_ty(&field.ty))
        .collect()
}

pub(in crate::jvm) fn class_ctor_jvm_tys(class: &IrClass) -> Vec<Ty> {
    // A constructor parameter is a value slot. `Unit` is the `kotlin.Unit` singleton there;
    // `V` is only a method result and is not a legal parameter descriptor.
    if class.ctor_args.is_empty() {
        jvm_tys(
            &class.fields[..class.ctor_param_count as usize]
                .iter()
                .map(|field| field.ty)
                .collect::<Vec<_>>(),
        )
    } else {
        jvm_tys(
            &class
                .ctor_args
                .iter()
                .map(|argument| argument.ty)
                .collect::<Vec<_>>(),
        )
    }
}
