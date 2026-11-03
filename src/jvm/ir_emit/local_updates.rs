//! JVM representation of source-local updates.

use super::{CodeBuilder, Emitter};
use crate::ir::{IrBinOp, IrConst, IrExpr, IrFile, IrTypeOp};
use crate::types::Ty;

/// Recognize an `Int` local's small constant self-update, which the JVM represents directly as
/// `iinc`. Prefix and postfix increments in statement position carry an identity coercion around
/// the same checked arithmetic as `+= 1`; that semantic wrapper does not change the update. A
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
    iinc_delta_of(ir, value, var)
}

fn iinc_delta_of(ir: &IrFile, value: u32, var: u32) -> Option<i8> {
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

/// A recorded postfix update `{ val old = x; x = old + c; old }` of local `x`.
fn postfix_update(ir: &IrFile, block: u32) -> Option<(u32, i8)> {
    if !ir.postfix_updates.contains(&block) {
        return None;
    }
    let IrExpr::Block {
        stmts,
        value: Some(value),
    } = ir.expr(block)
    else {
        return None;
    };
    let [declaration, update] = stmts[..] else {
        return None;
    };
    let IrExpr::Variable {
        index: old,
        init: Some(initializer),
        ..
    } = *ir.expr(declaration)
    else {
        return None;
    };
    let IrExpr::GetValue(target) = *ir.expr(initializer) else {
        return None;
    };
    let IrExpr::SetValue {
        var: written,
        value: updated,
    } = *ir.expr(update)
    else {
        return None;
    };
    if written != target || !matches!(ir.expr(*value), IrExpr::GetValue(read) if *read == old) {
        return None;
    }
    Some((target, iinc_delta_of(ir, updated, old)?))
}

impl Emitter<'_> {
    /// Emit the JVM contraction of a source postfix update after its ordinary block scope has
    /// opened. The caller still owns every block/debug close step.
    pub(super) fn emit_postfix_iinc(
        &mut self,
        block: u32,
        discarded: bool,
        code: &mut CodeBuilder,
    ) -> bool {
        let Some((target, delta)) = postfix_update(self.ir, block) else {
            return false;
        };
        let Some(&(slot, ty)) = self.slots.get(&target) else {
            return false;
        };
        if ty != Ty::Int || self.inline_return_frame_result_type(target).is_some() {
            return false;
        }
        // kotlinc's optimization lowering leaves a statement `n++` as the store `n = n.inc()` at
        // the update's offsets, whose value marks the update's line: the `iinc` keeps it.
        self.mark_expression_start(block, code);
        if !discarded {
            code.iload(slot);
        }
        code.iinc(slot, delta);
        true
    }
}
