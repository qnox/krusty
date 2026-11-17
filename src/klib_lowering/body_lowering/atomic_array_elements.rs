//! The target stdlib's atomic accesses to one array element.

use super::*;
use crate::ir::IrCheckedOperation;
use crate::libraries::AtomicOperation;
use crate::metadata::klib_ir::tree::KlibIrMemberAccess;

impl BodyLowering<'_> {
    /// The atomic access `operation` to the element at the call's index of its receiver array.
    /// The array, the index and then the operands are evaluated in order; each operand is the
    /// array's element.
    pub(super) fn atomic_array_element(
        &mut self,
        operation: AtomicOperation,
        access: &KlibIrMemberAccess,
        ty: Ty,
    ) -> Lowered<ExprId> {
        let arguments = super::calls::arguments(access)
            .into_iter()
            .collect::<Option<Vec<_>>>();
        let Some([array, index, operands @ ..]) = arguments.as_deref() else {
            return Err(mismatch(
                "an atomic array element access has no array or index",
            ));
        };
        if operands.len() != operation.operands() {
            return Err(mismatch(
                "an atomic array element access has another shape than its declaration's",
            ));
        }
        let array_ty = self.ty(self.arena.expr(*array), "atomic array")?;
        let Some(element) = array_ty
            .is_array()
            .then(|| array_ty.array_read_elem())
            .flatten()
        else {
            return Err(mismatch(
                "an atomic array element access is not on an array",
            ));
        };
        let result = match operation {
            AtomicOperation::Store => Ty::Unit,
            AtomicOperation::CompareAndSet => Ty::Boolean,
            AtomicOperation::Load
            | AtomicOperation::Exchange
            | AtomicOperation::CompareAndExchange
            | AtomicOperation::FetchAndAdd => element,
        };
        if ty != result {
            return Err(mismatch(
                "an atomic array element access disagrees with its array's element",
            ));
        }
        let array = self.operand(*array, array_ty)?;
        let index = self.operand(*index, Ty::Int)?;
        let operands = operands
            .iter()
            .map(|&operand| self.operand(operand, element))
            .collect::<Lowered<Vec<_>>>()?;
        let lowered = self.ir.add_expr(IrExpr::Checked(Box::new(
            IrCheckedOperation::AtomicArrayElement {
                operation,
                array,
                index,
                operands,
            },
        )));
        Ok(self.typed(lowered, ty))
    }
}
