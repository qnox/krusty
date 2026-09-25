//! kotlinc's nullability annotations on an abstract (bodiless) method.
//!
//! Having no body is why an abstract method gets no debug tables; it still carries `@NotNull` /
//! `@Nullable` on each reference parameter and on a reference return.

use crate::ir::{IrFile, IrFunction};
use crate::types::Ty;

const NULLABLE: &str = "Lorg/jetbrains/annotations/Nullable;";
const NOT_NULL: &str = "Lorg/jetbrains/annotations/NotNull;";

/// The result annotation and one annotation per parameter of `function`.
pub(super) fn annotations(
    ir: &IrFile,
    fid: u32,
    function: &IrFunction,
) -> (Option<&'static str>, Vec<Option<&'static str>>) {
    // Declared parameter nullability lives in the side-table (kept off `params` for the mangle).
    let declared_nullable = ir.fn_param_declared_nullable.get(&fid);
    let parameters = function
        .params
        .iter()
        .enumerate()
        .map(|(index, &ty)| {
            if declares_nullable_bounded_type_parameter(ir, fid, Some(index)) {
                None
            } else if declared_nullable
                .and_then(|flags| flags.get(index))
                .copied()
                .unwrap_or(false)
            {
                annotation(Ty::nullable(ty))
            } else {
                annotation(ty)
            }
        })
        .collect();
    let result = (!declares_nullable_bounded_type_parameter(ir, fid, None))
        .then(|| annotation(function.ret))
        .flatten();
    (result, parameters)
}

fn annotation(ty: Ty) -> Option<&'static str> {
    // A bare type parameter erases to its bound, and kotlinc annotates it only when that bound is
    // NON-NULL. `<T>` carries the implicit `Any?` bound and can be instantiated with a nullable type,
    // so neither `@NotNull` nor `@Nullable` is true of the position; `<T : Any>` is known non-null and
    // gets `@NotNull`. The bound travels on the type itself, so this needs no signature lookup.
    if let Ty::TyParam(_, bound) = ty {
        if matches!(bound, Ty::Nullable(_)) {
            return None;
        }
    }
    let descriptor = crate::jvm::names::type_descriptor(ty);
    if !(descriptor.starts_with('L') || descriptor.starts_with('[')) {
        return None;
    }
    Some(if matches!(ty, Ty::Nullable(_)) {
        NULLABLE
    } else {
        NOT_NULL
    })
}

/// A backend pass may already have erased a type parameter to what its bound is realized as (a
/// value-class bound becomes its carrier). The declared signature still says it was a type
/// parameter, and one with a nullable bound stays unannotated. `None` asks about the result.
fn declares_nullable_bounded_type_parameter(
    ir: &IrFile,
    fid: u32,
    position: Option<usize>,
) -> bool {
    let declared = |(parameters, result): (&[Ty], Ty)| match position {
        Some(index) => parameters.get(index).copied(),
        None => Some(result),
    };
    ir.signatures
        .get(&fid)
        .and_then(|signature| declared((&signature.params, signature.ret.unwrap_or(Ty::Unit))))
        .into_iter()
        .chain(
            ir.member_semantic_sigs
                .get(&fid)
                .and_then(|(parameters, result)| declared((parameters, *result))),
        )
        .any(|ty| matches!(ty, Ty::TyParam(_, bound) if bound.is_nullable()))
}
