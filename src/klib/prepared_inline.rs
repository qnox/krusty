//! The shape of a prepared inline copy, as Kotlin 2.4 serializes one into
//! `default/ir_inlinable_functions/`: a private package-level copy of the inline function whose
//! dispatch receiver is its first regular parameter, `<this>`, and whose non-reified type
//! parameters are erased to their first upper bound wherever a value's type names them. The
//! copy's own identity is signed from that shape; the declaration table still keys it by the
//! original declaration's identity.

use crate::ir::IrTypeParameter;
use crate::types::{Ty, TyKind};

/// The types a prepared copy writes for the type parameters `parameters` of a function's checked
/// signature: each non-reified one, by name, erased to its first upper bound, `Any?` without one.
pub(super) fn erasures(parameters: &[IrTypeParameter]) -> Vec<(String, Ty)> {
    parameters
        .iter()
        .filter(|parameter| !parameter.reified)
        .map(|parameter| (parameter.semantic_name.clone(), parameter.upper_bound()))
        .collect()
}

/// `ty` as a prepared copy writes it, given `erasures`: an erased type parameter's occurrence is
/// its bound, nullable where the occurrence is. Other types are written as they are; the type
/// table erases their arguments as it writes each one.
pub(super) fn erase(ty: Ty, erasures: &[(String, Ty)]) -> Ty {
    let (inner, nullable) = match ty.kind() {
        TyKind::Nullable(inner) => (inner, true),
        _ => (ty, false),
    };
    let TyKind::TyParam(name, _) = inner.kind() else {
        return ty;
    };
    match erasures
        .iter()
        .find(|(parameter, _)| parameter == name.as_str())
    {
        Some((_, bound)) if nullable => Ty::nullable(*bound),
        Some((_, bound)) => *bound,
        None => ty,
    }
}
