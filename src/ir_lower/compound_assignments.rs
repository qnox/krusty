//! Lowering of checker-selected augmented-assignment convention calls.

use super::*;

impl Lower<'_> {
    pub(super) fn lower_plus_assign(
        &mut self,
        value: AstExprId,
        target: CompoundAssignmentTarget,
    ) -> Option<u32> {
        let Expr::Binary { op, lhs, rhs, .. } = self.afile.expr(value).clone() else {
            return None;
        };
        self.lower_plus_assign_operands(op, lhs, rhs, target)
    }

    pub(super) fn lower_plus_assign_operands(
        &mut self,
        operation: BinOp,
        receiver: AstExprId,
        argument: AstExprId,
        target: CompoundAssignmentTarget,
    ) -> Option<u32> {
        let name = match operation {
            BinOp::Add => "plusAssign",
            BinOp::Sub => "minusAssign",
            BinOp::Mul => "timesAssign",
            BinOp::Div => "divAssign",
            BinOp::Rem => "remAssign",
            _ => return None,
        };
        let receiver_ty = self.info.ty(receiver);
        let receiver = self.expr(receiver)?;
        self.lower_selected_op_call(
            receiver,
            receiver_ty,
            name,
            &[argument],
            *target.call,
            None,
            &[],
            None,
        )
        .map(|(call, _)| call)
    }
}
