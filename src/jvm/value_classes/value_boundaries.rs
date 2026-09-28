//! The box or unbox a value crossing into a parameter, a default or an array element needs, so
//! every path reaching the boundary arrives in the representation the destination takes.

use super::{erase, is_ref, target, BoxOp, Repr, ReprCtx, Target, Under};
use crate::ir::{value_tails, Callee, ExprId, IrExpr};
use crate::types::Ty;

pub(super) fn record_value_boundary(
    ops: &mut Vec<(ExprId, BoxOp)>,
    exprs: &[IrExpr],
    repr_ctx: &ReprCtx<'_>,
    value: ExprId,
    parameter: Ty,
    under: &Under,
) {
    let target = target(&parameter, under);
    let representation = repr_ctx.repr(value);
    crate::trace_compiler!(
        "value_classes",
        "boundary expr {value} {:?} -> param {parameter:?} repr={} target={}",
        &exprs[value as usize],
        match representation {
            Repr::Unboxed(_) => "Unboxed",
            Repr::Boxed(_) => "Boxed",
            Repr::NotVc => "NotVc",
        },
        match target {
            Target::UnboxedX(_) => "UnboxedX",
            Target::Boxed => "Boxed",
            Target::Other => "Other",
        }
    );
    let supertype_box = matches!(target, Target::Boxed)
        || (matches!(target, Target::Other)
            && is_ref(&parameter)
            && match representation {
                Repr::Unboxed(value_class) | Repr::Boxed(value_class) => {
                    let underlying = under
                        .get(&value_class)
                        .map(|underlying| erase(underlying, under).non_null());
                    let own_underlying = underlying.as_ref() == Some(&parameter.non_null())
                        && underlying
                            .as_ref()
                            .and_then(|ty| ty.obj_internal())
                            .is_none_or(|name| !name.matches("java/lang/Object"));
                    parameter.non_null().obj_internal() != Some(value_class) && !own_underlying
                }
                Repr::NotVc => false,
            });
    match representation {
        // A boxed branch result may still contain an unboxed value-class tail. Box that tail so
        // every path entering the merge has the same representation.
        Repr::Unboxed(value_class) | Repr::Boxed(value_class) if supertype_box => {
            let mut tails = Vec::new();
            value_tails(exprs, value, &mut tails);
            for tail in tails {
                if matches!(repr_ctx.repr(tail), Repr::Unboxed(tail_class) if tail_class == value_class)
                {
                    ops.push((tail, repr_ctx.box_op(tail, value_class)));
                }
            }
        }
        Repr::Boxed(value_class) if matches!(target, Target::UnboxedX(target_class) if target_class == value_class) =>
        {
            let mut tails = Vec::new();
            value_tails(exprs, value, &mut tails);
            for tail in tails {
                if matches!(repr_ctx.repr(tail), Repr::Boxed(tail_class) if tail_class == value_class)
                {
                    ops.push((tail, BoxOp::unbox(value_class, parameter.is_nullable())));
                }
            }
        }
        Repr::NotVc => {
            if let Target::UnboxedX(value_class) = target {
                if matches!(
                    &exprs[value as usize],
                    IrExpr::Call {
                        callee: Callee::Intrinsic { .. },
                        ..
                    }
                ) {
                    ops.push((value, BoxOp::unbox(value_class, parameter.is_nullable())));
                }
            }
        }
        _ => {}
    }
}

/// A Kotlin `Array<T>` is a JVM reference array even when `T` is a non-null value class. Its semantic
/// element type stays `T`; only this platform boundary requires the boxed `T` object for `aastore`.
pub(super) fn record_reference_array_element_boundary(
    ops: &mut Vec<(ExprId, BoxOp)>,
    exprs: &[IrExpr],
    repr_ctx: &ReprCtx<'_>,
    value: ExprId,
    element: Ty,
) {
    let Some(value_class) = element
        .non_null()
        .obj_internal()
        .filter(|name| repr_ctx.under.contains_key(name))
    else {
        return;
    };
    if !matches!(repr_ctx.repr(value), Repr::Unboxed(actual) if actual == value_class) {
        return;
    }
    let mut tails = Vec::new();
    value_tails(exprs, value, &mut tails);
    for tail in tails {
        if matches!(repr_ctx.repr(tail), Repr::Unboxed(actual) if actual == value_class) {
            ops.push((tail, repr_ctx.box_op(tail, value_class)));
        }
    }
}
