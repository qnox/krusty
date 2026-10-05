//! Which operands of a call reordered by its named arguments keep their place.

use super::super::BodyLowering;
use crate::ir::{ExprId, IrCheckedOperation, IrExpr, IrTypeOp};
use crate::types::Ty;

impl BodyLowering<'_> {
    /// Whether a call whose named arguments reorder it passes `value` in place, as kotlinc does
    /// for an operand with no effect to reorder: a constant, a read of a `val`, a parameter or a
    /// receiver, a function literal that is not SAM-converted, an unbound callable reference, or an
    /// unbound class literal. Every other operand is first stored in a temporary, in source order.
    pub(in super::super) fn passes_reordered_operand_in_place(&self, value: ExprId) -> bool {
        match self.ir.expr(value) {
            // A function literal's and a reference's captures are reads of the enclosing body's
            // locals and shared cells, which kotlinc passes in place with the literal. A bound
            // reference evaluates its receiver where it is written, so it is stored.
            IrExpr::Const(_)
            | IrExpr::KClassLiteral { value: None, .. }
            | IrExpr::Lambda { sam: None, .. } => true,
            IrExpr::CallableReference(reference) => reference.bound_receiver.is_none(),
            IrExpr::TypeOp {
                op: IrTypeOp::ImplicitCoercion,
                arg,
                ..
            } => self.passes_reordered_operand_in_place(*arg),
            IrExpr::Checked(IrCheckedOperation::PropertyReference {
                dispatch_receiver: None,
                extension_receiver: None,
                ..
            }) => true,
            IrExpr::GetValue(slot) => {
                self.stable_value_read(value).is_some()
                    || self.receiver_slots().any(|receiver| receiver == *slot)
            }
            _ => false,
        }
    }

    /// Store a reordered call's operand in a temporary of the type it evaluates to, as kotlinc's
    /// temporary holds it, and coerce it to the parameter where it is passed: a scalar the parameter
    /// takes as a reference is boxed there, not before it is stored, and a bound reference is held at
    /// its own reflective type (`KFunction0`, `KProperty0`).
    pub(in super::super) fn spill_reordered_operand(
        &mut self,
        value: ExprId,
        parameter_ty: Ty,
        statements: &mut Vec<ExprId>,
    ) -> ExprId {
        if let &IrExpr::TypeOp {
            op: IrTypeOp::ImplicitCoercion,
            arg,
            type_operand,
        } = self.ir.expr(value)
        {
            if let Some(&operand_ty) = self.ir.logical_types.get(&arg) {
                let boxed = !operand_ty.is_reference() && type_operand.is_reference();
                if boxed || self.is_reference_value(arg) {
                    let read = self.spill_call_operand(arg, operand_ty, statements);
                    return self.ir.add_expr(IrExpr::TypeOp {
                        op: IrTypeOp::ImplicitCoercion,
                        arg: read,
                        type_operand,
                    });
                }
            }
        }
        if self.is_reference_value(value) {
            if let Some(&reference_ty) = self.ir.logical_types.get(&value) {
                return self.spill_call_operand(value, reference_ty, statements);
            }
        }
        self.spill_call_operand(value, parameter_ty, statements)
    }

    fn is_reference_value(&self, value: ExprId) -> bool {
        matches!(
            self.ir.expr(value),
            IrExpr::CallableReference(_)
                | IrExpr::Checked(IrCheckedOperation::PropertyReference { .. })
        )
    }
}
