//! The box or unbox a value crossing into a parameter, a default or an array element needs, so
//! every path reaching the boundary arrives in the representation the destination takes.

use super::{erase, is_ref, target, BoxOp, Repr, ReprCtx, Target, Under};
use crate::ir::{value_tails, Callee, ExprId, IrExpr};
use crate::types::Ty;

/// The receiver of a property access `id` crosses into its selected accessor. A value-class
/// property accessor is a static `-impl` over the unboxed carrier, regardless of whether this
/// compilation or a dependency declared it: the sole stored property was already rewritten to
/// identity, and every remaining read keeps the carrier its accessor expects. Any other owner's
/// accessor — an interface property a value class inherits, read or written through the value
/// class — dispatches on the box.
pub(super) fn record_property_receiver_boundary(
    ops: &mut Vec<(ExprId, BoxOp)>,
    exprs: &[IrExpr],
    repr_ctx: &ReprCtx<'_>,
    id: ExprId,
    under: &Under,
) {
    let (receiver, owner, read) = match &exprs[id as usize] {
        IrExpr::PropertyRead {
            receiver: Some(receiver),
            owner,
            ..
        } => (*receiver, owner, true),
        IrExpr::PropertyWrite {
            receiver: Some(receiver),
            owner,
            ..
        } => (*receiver, owner, false),
        _ => return,
    };
    match repr_ctx.repr(receiver) {
        Repr::Boxed(value_class) if read && under.contains_key(owner) => {
            ops.push((receiver, BoxOp::Unbox(value_class)));
        }
        Repr::Unboxed(value_class) if !under.contains_key(owner) => {
            ops.push((receiver, repr_ctx.box_op(receiver, value_class)));
        }
        _ => {}
    }
}

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
                    let own_underlying = underlying
                        .is_some_and(|ty| ty == parameter.non_null() && !ty.is_erased_top());
                    parameter.non_null().obj_internal() != Some(value_class) && !own_underlying
                }
                Repr::NotVc => false,
            });
    match representation {
        // A boxed branch result may still contain an unboxed value-class tail. Box that tail so
        // every path entering the merge has the same representation.
        Repr::Unboxed(value_class) | Repr::Boxed(value_class) if supertype_box => {
            if let Some(op) = carrier_join_box(exprs, repr_ctx, value, value_class, under) {
                ops.push((value, op));
                return;
            }
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
                        callee: call_callee,
                        ..
                    } if matches!(&**call_callee, Callee::Intrinsic { .. })
                ) {
                    ops.push((value, BoxOp::unbox(value_class, parameter.is_nullable())));
                }
            }
        }
        _ => {}
    }
}

/// The box a value whose `when` joins on the carrier of `value_class` takes as a whole. When every
/// branch (a safe call `x?.id` included) yields that carrier or `null`, the merge holds the carrier,
/// so boxing a branch tail would only be unboxed again there. Like kotlinc, box the joined value
/// once; a nullable `X?` keeps its `null` (`dup; ifnull`), a non-null `X` takes plain `box-impl`.
/// A join realized on the box itself (`X?` over a primitive carrier), or one with any other branch,
/// returns `None`: its branches are boxed individually.
fn carrier_join_box(
    exprs: &[IrExpr],
    repr_ctx: &ReprCtx<'_>,
    value: ExprId,
    value_class: crate::types::TypeName,
    under: &Under,
) -> Option<BoxOp> {
    let mut join = value;
    while let IrExpr::Block {
        value: Some(inner), ..
    } = &exprs[join as usize]
    {
        join = *inner;
    }
    if !matches!(exprs[join as usize], IrExpr::When { .. }) {
        return None;
    }
    let mut tails = Vec::new();
    value_tails(exprs, join, &mut tails);
    let mut nullable = false;
    for tail in tails {
        if repr_ctx.operand_null_only(tail) {
            nullable = true;
        } else if matches!(repr_ctx.repr(tail), Repr::Unboxed(tail_class) if tail_class == value_class)
        {
            nullable |= !repr_ctx.operand_nonnull(tail);
        } else {
            return None;
        }
    }
    match repr_ctx.types.join_result(join) {
        // An exhaustive `when` (a safe call's guard included) merges on its checked result, erased.
        // That result is `X?` only when its erasure is a nullable form of a non-null carrier: over
        // a nullable carrier (`V(val value: String?)`) `V?` is the box, so a carrier merge there is
        // a non-null `V` whose carrier may itself be `null` (`V(null)` is still boxed).
        Some(merge) => {
            let carrier = erase(under.get(&value_class)?, under);
            if merge.non_null() != carrier.non_null() {
                return None;
            }
            nullable = merge.is_nullable() && !carrier.is_nullable();
        }
        // An `if` merges on the representation its branches share, which holds a `null` only
        // when the carrier can.
        None if nullable && super::nullable_is_boxed(value_class, under) => return None,
        None => {}
    }
    Some(if nullable {
        BoxOp::BoxNull(value_class)
    } else {
        BoxOp::Box(value_class)
    })
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
