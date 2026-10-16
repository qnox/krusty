//! kotlinc's nullability annotations on an abstract (bodiless) method.
//!
//! Having no body is why an abstract method gets no debug tables; it still carries `@NotNull` /
//! `@Nullable` on each reference parameter and on a reference return, a suspend one's always
//! `@Nullable`.

use crate::ir::{IrFile, IrFunction};
use crate::jvm::declared_type_nullability::{declared_annotation, is_unannotated_type_parameter};
use crate::types::Ty;

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
    // A suspend function's result is its CPS `Object`: the value or `COROUTINE_SUSPENDED`, which
    // kotlinc annotates `@Nullable` whatever the declared result.
    let result = if ir.suspend_funs.contains(&fid) {
        annotation(Ty::nullable(function.ret))
    } else {
        (!declares_nullable_bounded_type_parameter(ir, fid, None))
            .then(|| annotation(function.ret))
            .flatten()
    };
    (result, parameters)
}

/// A position's annotation from its own type, which still carries a type parameter's bound.
fn annotation(ty: Ty) -> Option<&'static str> {
    declared_annotation(ty, ty)
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
        .any(is_unannotated_type_parameter)
}
