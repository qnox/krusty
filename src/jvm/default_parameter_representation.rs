//! JVM representation facts shared by default-stub realization and emission.
//!
//! A primitive-bounded Kotlin type parameter has two physical forms at this boundary: its real
//! method uses the primitive bound, while its `$default` stub uses the corresponding JDK wrapper.

use crate::jvm::physical_type::ir_ty_to_jvm;
use crate::types::Ty;

/// `(primitive, JDK wrapper)` when `declared` is a non-null type parameter bounded by a JVM
/// primitive.
pub(super) fn primitive_bounded_type_parameter(declared: Ty) -> Option<(Ty, Ty)> {
    let Ty::TyParam(_, bound) = declared.non_null() else {
        return None;
    };
    if declared.is_nullable() {
        return None;
    }
    let primitive = ir_ty_to_jvm(bound);
    primitive_wrapper(primitive).map(|wrapper| (primitive, wrapper))
}

pub(super) fn primitive_wrapper(primitive: Ty) -> Option<Ty> {
    let internal = match primitive {
        Ty::Boolean => "java/lang/Boolean",
        Ty::Byte => "java/lang/Byte",
        Ty::Short => "java/lang/Short",
        Ty::Char => "java/lang/Character",
        Ty::Int => "java/lang/Integer",
        Ty::Long => "java/lang/Long",
        Ty::Float => "java/lang/Float",
        Ty::Double => "java/lang/Double",
        _ => return None,
    };
    Some(Ty::obj(internal))
}

pub(super) fn primitive_unbox_method(primitive: Ty) -> Option<&'static str> {
    match primitive {
        Ty::Boolean => Some("booleanValue"),
        Ty::Byte => Some("byteValue"),
        Ty::Short => Some("shortValue"),
        Ty::Char => Some("charValue"),
        Ty::Int => Some("intValue"),
        Ty::Long => Some("longValue"),
        Ty::Float => Some("floatValue"),
        Ty::Double => Some("doubleValue"),
        _ => None,
    }
}
