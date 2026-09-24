//! JVM representation of source-local updates.

use crate::ir::{IrBinOp, IrConst, IrExpr, IrFile, IrTypeOp};
use crate::types::Ty;

/// Recognize an `Int` local's small constant self-update, which the JVM represents directly as
/// `iinc`. Prefix and postfix increments in statement position carry an identity coercion around
/// the same checked arithmetic as `+= 1`; that semantic wrapper does not change the update.
pub(super) fn iinc_delta(ir: &IrFile, var: u32, value: u32, local_ty: Ty) -> Option<i8> {
    if local_ty != Ty::Int {
        return None;
    }

    let arithmetic = match *ir.expr(value) {
        IrExpr::TypeOp {
            op: IrTypeOp::ImplicitCoercion,
            arg,
            type_operand: Ty::Int,
        } => arg,
        _ => value,
    };
    let IrExpr::PrimitiveBinOp { op, lhs, rhs } = *ir.expr(arithmetic) else {
        return None;
    };
    let constant = |expr| match ir.expr(expr) {
        IrExpr::Const(IrConst::Int(value)) => Some(*value),
        _ => None,
    };
    let is_local = |expr| matches!(ir.expr(expr), IrExpr::GetValue(index) if *index == var);

    let delta = match op {
        IrBinOp::Add if is_local(lhs) => constant(rhs),
        IrBinOp::Add if is_local(rhs) => constant(lhs),
        IrBinOp::Sub if is_local(lhs) => constant(rhs).and_then(i32::checked_neg),
        _ => None,
    }?;
    i8::try_from(delta).ok()
}
