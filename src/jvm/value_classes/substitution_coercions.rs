//! A checked coercion from a generic slot to its substituted value class (`c.a` read as `R<Int>`
//! from `R<R<Int>>`) unboxes the box that slot holds. When the coerced value is consumed by a
//! reference slot that boxes it again (`fun read(c: R<R<Int>>): Any? = c.a`), kotlinc coerces from
//! the slot's erased type straight to the consumer and emits neither call. The coercion is then an
//! identity over the box. A nullable generic slot (`shelf[0]` of a `Shelf<X?>`) is the same
//! identity: its null-safe unbox and null-safe box each keep `null` as it is.
//!
//! When nothing consumes the coerced value at all (`id(ic)` as a statement), kotlinc's codegen
//! discards the call's own result as it is: `PromisedValue.discard` pops the callee's erased
//! result type and never coerces it, so neither the `checkcast` to the box nor `unbox-impl` is
//! emitted. The coercion is then the call itself.

use super::BoxOp;
use crate::fx_hash::{HashMap, HashSet};
use crate::ir::{ExprId, IrExpr, IrFile, IrTypeOp};

/// Drop every value-class conversion of a coercion kotlinc never materializes: an `unbox-impl`
/// its consumer boxes straight back, and one of a value nothing consumes.
pub(super) fn drop_unmaterialized(ops: &mut Vec<(ExprId, BoxOp)>, ir: &mut IrFile) {
    drop_rebox_round_trips(ops, ir);
    drop_discarded_unboxes(ops, ir);
}

/// Drop each `unbox-impl` of a coercion's operand that the coercion's consumer immediately boxes
/// back into the same value class, together with that `box-impl`, and make the coercion the
/// operand itself: it no longer converts to the carrier it was retargeted to.
fn drop_rebox_round_trips(ops: &mut Vec<(ExprId, BoxOp)>, ir: &mut IrFile) {
    let unboxes: HashSet<(ExprId, BoxOp)> = ops
        .iter()
        .copied()
        .filter(|(_, op)| matches!(op, BoxOp::Unbox(_) | BoxOp::UnboxNull(_)))
        .collect();
    let mut identities = HashSet::default();
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
        let unbox = match op {
            BoxOp::Box(_) => BoxOp::Unbox(value_class),
            BoxOp::BoxNull(_) if unboxes.contains(&(arg, BoxOp::UnboxNull(value_class))) => {
                BoxOp::UnboxNull(value_class)
            }
            BoxOp::BoxNull(_) => BoxOp::Unbox(value_class),
            _ => unreachable!("guarded box operation"),
        };
        if unboxes.contains(&(arg, unbox)) {
            identities.insert((coercion, op));
            identities.insert((arg, unbox));
            ir.exprs[coercion as usize] = IrExpr::Block {
                stmts: vec![],
                value: Some(arg),
            };
            ir.take_physical_type(coercion);
        }
    }
    ops.retain(|operation| !identities.contains(operation));
}

/// Drop each `unbox-impl` of a coercion whose value is discarded, and make that coercion its
/// operand: the discarded value is the operand's own result, popped as it is.
fn drop_discarded_unboxes(ops: &mut Vec<(ExprId, BoxOp)>, ir: &mut IrFile) {
    let discarded = discarded_coercions(ir);
    let mut identities = Vec::new();
    ops.retain(|&(operand, op)| {
        let coercion = matches!(op, BoxOp::Unbox(_) | BoxOp::UnboxNull(_))
            .then(|| discarded.get(&operand))
            .flatten();
        if let Some(&coercion) = coercion {
            identities.push((coercion, operand));
        }
        coercion.is_none()
    });
    for (coercion, operand) in identities {
        ir.exprs[coercion as usize] = IrExpr::Block {
            stmts: vec![],
            value: Some(operand),
        };
        ir.take_physical_type(coercion);
    }
}

/// The implicit coercions whose value nothing reads, by their operand: a block's statements, and
/// the value of a block that is itself discarded.
fn discarded_coercions(ir: &IrFile) -> HashMap<ExprId, ExprId> {
    let mut pending: Vec<ExprId> = ir
        .exprs
        .iter()
        .filter_map(|expression| match expression {
            IrExpr::Block { stmts, .. } => Some(stmts.iter().copied()),
            _ => None,
        })
        .flatten()
        .collect();
    let mut visited = HashSet::default();
    let mut coercions = HashMap::default();
    while let Some(id) = pending.pop() {
        if !visited.insert(id) {
            continue;
        }
        match ir.exprs[id as usize] {
            IrExpr::Block {
                value: Some(value), ..
            } => pending.push(value),
            IrExpr::TypeOp {
                op: IrTypeOp::ImplicitCoercion,
                arg,
                ..
            } => {
                coercions.insert(arg, id);
            }
            _ => {}
        }
    }
    coercions
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{type_name, Ty};

    fn coercion(ir: &mut IrFile) -> (ExprId, ExprId) {
        let argument = ir.add_expr(IrExpr::GetValue(0));
        let coercion = ir.add_expr(IrExpr::TypeOp {
            op: IrTypeOp::ImplicitCoercion,
            arg: argument,
            type_operand: Ty::nullable(Ty::obj("kotlin/Any")),
        });
        (argument, coercion)
    }

    #[test]
    fn nullable_unbox_is_not_cancelled_by_a_non_null_box() {
        let mut ir = IrFile::default();
        let (argument, coercion) = coercion(&mut ir);
        let value_class = type_name("fixture/Value");
        let mut operations = vec![
            (argument, BoxOp::UnboxNull(value_class)),
            (coercion, BoxOp::Box(value_class)),
        ];

        drop_rebox_round_trips(&mut operations, &mut ir);

        assert_eq!(operations.len(), 2);
        assert!(matches!(
            ir.expr(coercion),
            IrExpr::TypeOp {
                op: IrTypeOp::ImplicitCoercion,
                arg,
                ..
            } if *arg == argument
        ));
    }

    #[test]
    fn nullable_unbox_and_nullable_box_cancel_as_one_identity() {
        let mut ir = IrFile::default();
        let (argument, coercion) = coercion(&mut ir);
        let value_class = type_name("fixture/Value");
        let mut operations = vec![
            (argument, BoxOp::UnboxNull(value_class)),
            (coercion, BoxOp::BoxNull(value_class)),
        ];

        drop_rebox_round_trips(&mut operations, &mut ir);

        assert!(operations.is_empty());
        assert!(matches!(
            ir.expr(coercion),
            IrExpr::Block {
                stmts,
                value: Some(value)
            } if stmts.is_empty() && *value == argument
        ));
    }
}
