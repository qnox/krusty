//! Erased reference boundaries for a dynamic invoke, a string template, and a vararg.
//!
//! Each element that enters an `Object` slot boxes. A spread contributes the array itself:
//! `IntSpreadBuilder.addSpread` reads the carrier (`int[]` for `UIntArray`), so boxing that
//! value class would hand the builder the box. A string-template part flows into
//! `StringBuilder.append(Object)` and boxes, unless it is a non-null unboxed value, which
//! kotlinc renders through the static `toString-impl` over its carrier.

use super::{BoxOp, Repr, ReprCtx};
use crate::ir::{ExprId, IrExpr};

pub(super) fn record(
    exprs: &[IrExpr],
    id: ExprId,
    repr_ctx: &ReprCtx<'_>,
    ops: &mut Vec<(ExprId, BoxOp)>,
) {
    let aggregate = match &exprs[id as usize] {
        IrExpr::InvokeFunction { args, .. } => Some((args.clone(), Vec::new(), false)),
        IrExpr::StringConcat(args) => Some((args.clone(), Vec::new(), true)),
        IrExpr::Vararg {
            elements, spreads, ..
        } => Some((elements.clone(), spreads.clone(), false)),
        _ => None,
    };
    let Some((args, spread_flags, template)) = aggregate else {
        return;
    };
    for (index, element) in args.into_iter().enumerate() {
        if spread_flags.get(index).copied().unwrap_or(false) {
            continue;
        }
        let representation = repr_ctx.repr(element);
        crate::trace_compiler!(
            "value_classes",
            "reference aggregate expr {id} element {element} {:?} repr={}",
            &exprs[element as usize],
            match representation {
                Repr::Unboxed(_) => "Unboxed",
                Repr::Boxed(_) => "Boxed",
                Repr::NotVc => "NotVc",
            }
        );
        if let Repr::Unboxed(value_class) = representation {
            let op = match repr_ctx.box_op(element, value_class) {
                BoxOp::Box(value_class) if template => BoxOp::StringOf(value_class),
                op => op,
            };
            ops.push((element, op));
        }
    }
}
