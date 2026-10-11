//! JVM representation of source-local updates.

use crate::ir::{IrBinOp, IrConst, IrExpr, IrFile, IrTypeOp};
use crate::types::Ty;

/// Recognize an `Int` local's small constant self-update, which the JVM represents directly as
/// `iinc`. Prefix and postfix increments in statement position carry an identity coercion around
/// the same checked arithmetic as `+= 1`; that semantic wrapper does not change the update. A
/// postfix update (`IrFile::postfix_updates`) adds to the temporary holding the local's old value,
/// which still equals the local, so kotlinc's `JvmOptimizationLowering` updates it in place too. A
/// lowering's plain update (`IrFile::plain_updates`) is not a source increment and stays one.
pub(super) fn iinc_delta(
    ir: &IrFile,
    update: u32,
    var: u32,
    value: u32,
    local_ty: Ty,
) -> Option<i8> {
    if local_ty != Ty::Int || ir.plain_updates.contains(&update) {
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
    let postfix = ir.postfix_updates.contains(&update);
    let is_local =
        |expr| matches!(ir.expr(expr), IrExpr::GetValue(index) if *index == var || postfix);

    let delta = match op {
        IrBinOp::Add if is_local(lhs) => constant(rhs),
        IrBinOp::Add if is_local(rhs) => constant(lhs),
        IrBinOp::Sub if is_local(lhs) => constant(rhs).and_then(i32::checked_neg),
        _ => None,
    }?;
    i8::try_from(delta).ok()
}

/// A postfix `x++`/`x--` of an `Int` local that the JVM realizes in place.
pub(super) struct PostfixIinc {
    /// The local's old value, read before the update when the block's value is used.
    pub(super) old_value: u32,
    pub(super) var: u32,
    pub(super) delta: i8,
}

/// Recognize a postfix update block `{ val tmp = x; x = tmp + d; tmp }` (or its statement form
/// without the trailing read) whose update is an `iinc`. kotlinc's `JvmOptimizationLowering`
/// rewrites it to read `x` and increment it in place, so the temporary never takes a slot.
pub(super) fn postfix_iinc(
    ir: &IrFile,
    stmts: &[u32],
    value: Option<u32>,
    local_ty: impl Fn(u32) -> Option<Ty>,
) -> Option<PostfixIinc> {
    let [saved, update] = *stmts else {
        return None;
    };
    let IrExpr::Variable {
        index: temporary,
        init: Some(old_value),
        ..
    } = *ir.expr(saved)
    else {
        return None;
    };
    let IrExpr::GetValue(var) = *ir.expr(old_value) else {
        return None;
    };
    let IrExpr::SetValue {
        var: target,
        value: updated,
    } = *ir.expr(update)
    else {
        return None;
    };
    let returns_saved = match value {
        None => true,
        Some(value) => matches!(ir.expr(value), IrExpr::GetValue(index) if *index == temporary),
    };
    if target != var || !returns_saved || !ir.postfix_updates.contains(&update) {
        return None;
    }
    let delta = iinc_delta(ir, update, var, updated, local_ty(var)?)?;
    Some(PostfixIinc {
        old_value,
        var,
        delta,
    })
}
