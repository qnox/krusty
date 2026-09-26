//! Boxing a value-class result where the consumer's slot is erased: a lambda's `() -> T` and a
//! function returning `Any`. Each tail of the result that yields the unboxed carrier is boxed; a
//! lambda boxes only a primitive carrier, since a reference one already satisfies `Object`.

use super::{box_wrap, erase, is_ref, value_class_name, Under};
use crate::ir::{Callee, ExprId, IrExpr, IrFile};
use crate::types::{Ty, TypeName};

/// Box an unboxed value-class result at every tail position of `id` (recursing `when`/block/return
/// tails). `prim_only` (the lambda `() -> T` case) boxes only a primitive-underlying result — a
/// reference one already satisfies the erased `Object`; the `Any`-return case (`prim_only = false`)
/// boxes any, so an `is X`/`as X` on the result holds.
pub(super) fn box_vc_tail(
    ir: &mut IrFile,
    id: ExprId,
    under: &Under,
    rets: &[Ty],
    prim_only: bool,
) {
    match &ir.exprs[id as usize] {
        IrExpr::When { branches } => {
            let rs: Vec<ExprId> = branches.iter().map(|(_, r)| *r).collect();
            for r in rs {
                box_vc_tail(ir, r, under, rets, prim_only);
            }
        }
        IrExpr::Block { value: Some(v), .. } => {
            let v = *v;
            box_vc_tail(ir, v, under, rets, prim_only);
        }
        // A statement-only block (`{ … ; return x }`) tails on its last statement.
        IrExpr::Block { value: None, stmts } => {
            if let Some(&last) = stmts.last() {
                box_vc_tail(ir, last, under, rets, prim_only);
            }
        }
        IrExpr::Return(Some(v)) => {
            let v = *v;
            box_vc_tail(ir, v, under, rets, prim_only);
        }
        // A supertype return-coercion (`make(): W` → `Any?`) wraps the value — box the INNER value, so
        // the coercion then just widens the boxed `X` (a no-op), rather than boxing the coercion result.
        IrExpr::TypeOp {
            op: crate::ir::IrTypeOp::ImplicitCoercion,
            arg,
            ..
        } if !prim_only => {
            let arg = *arg;
            box_vc_tail(ir, arg, under, rets, prim_only);
        }
        _ => {
            if let Some(x) = unboxed_vc_class(&ir.exprs, rets, under, id, !prim_only) {
                if ir.has_external_value_class_name(x) {
                    return;
                }
                let prim = under
                    .get(&x)
                    .map(|u| !is_ref(&erase(u, under)))
                    .unwrap_or(false);
                if !prim_only || prim {
                    box_wrap(ir, id, x, under);
                }
            }
        }
    }
}

/// The value class an expr produces UNBOXED (a `constructor-impl`/`unbox-impl` result, or a local call
/// whose return type is a non-null value class), if any.
fn unboxed_vc_class(
    exprs: &[IrExpr],
    rets: &[Ty],
    under: &Under,
    id: ExprId,
    calls: bool,
) -> Option<TypeName> {
    match &exprs[id as usize] {
        IrExpr::Call {
            callee: Callee::Static { owner, name, .. },
            ..
        } if name == "constructor-impl" || name == "unbox-impl" => value_class_name(*owner, under),
        // A local call returning an unboxed value class — only considered when `calls` is set (the
        // `Any`-return case); the lambda case must NOT box these (they already satisfy `Object`).
        IrExpr::Call { callee, .. } if calls && callee.source_function().is_some() => match rets
            .get(
                callee
                    .source_function()
                    .expect("guarded same-file function call") as usize,
            ) {
            Some(Ty::Obj(fq_name, _)) if under.contains_key(fq_name) => Some(*fq_name),
            _ => None,
        },
        IrExpr::Block { value: Some(v), .. } => unboxed_vc_class(exprs, rets, under, *v, calls),
        IrExpr::NotNullAssert { operand, .. } if calls => {
            unboxed_vc_class(exprs, rets, under, *operand, calls)
        }
        _ => None,
    }
}
