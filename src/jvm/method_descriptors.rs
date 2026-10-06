//! JVM method descriptors shared by representation planning and emission.
//!
//! Planning and emission name the same physical method. `Nothing` and `Nothing?` are `Void` in a
//! declaration descriptor; a value position still erases nullable `Nothing` to `Any`.

use crate::jvm::names::method_descriptor;
use crate::jvm::physical_type::ir_ty_to_jvm;
use crate::types::Ty;

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

/// A parameter, field, local, or property accessor. `Unit` is the `kotlin.Unit` singleton there,
/// including when a type parameter erases to `Unit`. `void` is only a method result, so callers
/// that describe a return keep [`jvm_declared_ty`].
pub(crate) fn jvm_value_ty(ty: &Ty) -> Ty {
    match jvm_declared_ty(ty) {
        Ty::Unit => Ty::obj("kotlin/Unit"),
        other => other,
    }
}

pub(crate) fn jvm_tys(types: &[Ty]) -> Vec<Ty> {
    types.iter().map(jvm_value_ty).collect()
}

pub(in crate::jvm) fn ir_method_desc(parameters: &[Ty], result: &Ty) -> String {
    method_descriptor(&jvm_tys(parameters), jvm_declared_ty(result))
}
