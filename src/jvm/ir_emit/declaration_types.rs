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
        Some(name) if crate::types::wk::is_any_or_object(name) => true,
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
