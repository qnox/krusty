//! A checked coercion from a generic slot to its substituted value class (`c.a` read as `R<Int>`
//! from `R<R<Int>>`) unboxes the box that slot holds. When the coerced value is consumed by a
//! reference slot that boxes it again (`fun read(c: R<R<Int>>): Any? = c.a`), kotlinc coerces from
//! the slot's erased type straight to the consumer and emits neither call. The coercion is then an
//! identity over the box.

use super::BoxOp;
use crate::ir::{ExprId, IrExpr, IrFile, IrTypeOp};
use std::collections::HashSet;

/// Drop each `unbox-impl` of a coercion's operand that the coercion's consumer immediately boxes
/// back into the same value class, together with that `box-impl`, and make the coercion the
/// operand itself: it no longer converts to the carrier it was retargeted to.
pub(super) fn drop_rebox_round_trips(ops: &mut Vec<(ExprId, BoxOp)>, ir: &mut IrFile) {
    let unboxes: HashSet<(ExprId, BoxOp)> = ops
        .iter()
        .copied()
        .filter(|(_, op)| matches!(op, BoxOp::Unbox(_)))
        .collect();
    let mut identities = HashSet::new();
    for &(coercion, op) in ops.iter() {
        let (BoxOp::Box(value_class) | BoxOp::BoxNull(value_class)) = op else {
            continue;
        };
        let IrExpr::TypeOp {
            op: IrTypeOp::ImplicitCoercion,
            arg,
            ..
        } = ir.exprs[coercion as usize]
        else {
            continue;
        };
        if unboxes.contains(&(arg, BoxOp::Unbox(value_class))) {
            identities.insert((coercion, op));
            identities.insert((arg, BoxOp::Unbox(value_class)));
            ir.exprs[coercion as usize] = IrExpr::Block {
                stmts: vec![],
                value: Some(arg),
            };
            ir.physical_types.remove(&coercion);
        }
    }
    ops.retain(|operation| !identities.contains(operation));
}
