//! Erased reference boundaries for a dynamic invoke, a string template, and a vararg.
//!
//! Each element that enters an `Object` slot boxes. A spread contributes the array itself, so it
//! crosses into the vararg's own array type: `IntSpreadBuilder.addSpread` and `Arrays.copyOf` read
//! the carrier (`int[]` for `UIntArray`), and a spread that arrives as the box (a smart cast to
//! `UIntArray`, a nullable `UIntArray?` local) is unboxed first. A string-template part flows into
//! `StringBuilder.append(Object)` and boxes, unless it is a non-null unboxed value, which
//! kotlinc renders through the static `toString-impl` over its carrier.

use super::value_boundaries::record_value_boundary;
use super::{BoxOp, Repr, ReprCtx, Under};
use krusty::fx_hash::HashSet;
use krusty::ir::{ExprId, IrExpr};

pub(super) fn record(
    exprs: &[IrExpr],
    id: ExprId,
    repr_ctx: &ReprCtx<'_>,
    rendered_value_class_text: &HashSet<ExprId>,
    under: &Under,
    ops: &mut Vec<(ExprId, BoxOp)>,
) {
    let aggregate = match &exprs[id as usize] {
        IrExpr::InvokeFunction { args, .. } => Some((args.clone(), Vec::new(), None, false)),
        IrExpr::StringConcat(args) => Some((args.clone(), Vec::new(), None, true)),
        IrExpr::Vararg {
            array_type,
            elements,
            spreads,
        } => Some((elements.clone(), spreads.clone(), Some(*array_type), false)),
        _ => None,
    };
    let Some((args, spread_flags, array_type, template)) = aggregate else {
        return;
    };
    for (index, element) in args.into_iter().enumerate() {
        if spread_flags.get(index).copied().unwrap_or(false) {
            if let Some(array_type) = array_type {
                record_value_boundary(ops, exprs, repr_ctx, element, array_type, under);
            }
            continue;
        }
        let representation = repr_ctx.repr(element);
        krusty::trace_compiler!(
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
            // Synthesis already rendered this value-class carrier to text. The expression keeps
            // the nested class as its logical type so concat selects kotlinc's Object boundary,
            // but this exact provenance says the physical String must not be rendered again.
            if template && rendered_value_class_text.contains(&element) {
                continue;
            }
            let op = match repr_ctx.box_op(element, value_class) {
                BoxOp::Box(value_class) if template => BoxOp::StringOf(value_class),
                op => op,
            };
            ops.push((element, op));
        }
    }
}
