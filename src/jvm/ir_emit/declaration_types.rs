//! Physical JVM types and descriptors for common-IR declarations.

use super::*;

pub(in crate::jvm) fn jvm_declared_ty(ty: &Ty) -> Ty {
    fn is_nothing(ty: &Ty) -> bool {
        match ty {
            Ty::Nothing => true,
            Ty::Nullable(inner) | Ty::PlatformNullable(inner) => is_nothing(inner),
            Ty::Obj(name, _) => name.matches("kotlin/Nothing"),
            _ => false,
        }
    }
    if is_nothing(ty) {
        Ty::obj("java/lang/Void")
    } else {
        match ir_ty_to_jvm(ty) {
            Ty::Nothing => Ty::obj("java/lang/Void"),
            other => other,
        }
    }
}

pub(crate) fn jvm_tys(types: &[Ty]) -> Vec<Ty> {
    types
        .iter()
        .map(|ty| {
            if *ty == Ty::Unit {
                Ty::obj("kotlin/Unit")
            } else {
                jvm_declared_ty(ty)
            }
        })
        .collect()
}

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

/// The JVM descriptor a common-IR function is declared with.
pub(in crate::jvm) fn function_descriptor(ir: &IrFile, function: crate::ir::FunId) -> String {
    method_descriptor(
        &jvm_function_params(ir, function),
        jvm_declared_ty(&ir.functions[function as usize].ret),
    )
}

pub(super) fn jvm_is_erased_top(ty: Ty) -> bool {
    match ty.obj_internal() {
        Some(name) if name.matches("java/lang/Object") || name.matches("kotlin/Any") => true,
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

pub(in crate::jvm) fn ir_method_desc(parameters: &[Ty], result: &Ty) -> String {
    method_descriptor(&jvm_tys(parameters), jvm_declared_ty(result))
}

pub(in crate::jvm) fn class_ctor_jvm_tys(class: &IrClass) -> Vec<Ty> {
    if class.ctor_args.is_empty() {
        class.fields[..class.ctor_param_count as usize]
            .iter()
            .map(|field| jvm_declared_ty(&field.ty))
            .collect()
    } else {
        class
            .ctor_args
            .iter()
            .map(|argument| jvm_declared_ty(&argument.ty))
            .collect()
    }
}
