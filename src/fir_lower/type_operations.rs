//! Lower checked type operations whose semantics require control flow.

use crate::fir::FirExprId;
use crate::ir::{ExprId, IrExpr, IrTypeOp};
use crate::types::{stored_value_ty, Ty};

use super::{BodyLowering, FirLoweringFailure};

impl BodyLowering<'_> {
    /// Lower `value as? T` to one evaluation followed by an `is T` guard, a checked cast on the
    /// successful branch, and `null` otherwise. `IrTypeOp::SafeCast` cannot be a backend no-op: the
    /// guarded shape is language semantics and therefore belongs in common lowering.
    pub(super) fn safe_cast_expression(
        &mut self,
        operand: FirExprId,
        target: Ty,
    ) -> Result<ExprId, FirLoweringFailure> {
        let operand_type = self
            .body
            .expr(operand)
            .ok_or(FirLoweringFailure::MissingExpression(operand))?
            .ty
            .get();
        let mut value = self.expression(operand)?;
        let storage_type = if operand_type == Ty::Unit {
            let unit = self.ir.add_expr(IrExpr::UnitInstance);
            value = self.ir.add_expr(IrExpr::Block {
                stmts: vec![value],
                value: Some(unit),
            });
            Ty::obj("kotlin/Unit")
        } else if operand_type.scalar_value_repr().is_some() {
            let any = Ty::obj("kotlin/Any");
            value = self.ir.add_expr(IrExpr::TypeOp {
                op: IrTypeOp::ImplicitCoercion,
                arg: value,
                type_operand: any,
            });
            any
        } else {
            stored_value_ty(operand_type)
        };

        // A safe cast to a primitive tests and retains its nullable wrapper. Reference targets and
        // erased type parameters already have a reference representation of their own.
        let target = target.non_null();
        let runtime_target = if target == Ty::Unit {
            Ty::obj("kotlin/Unit")
        } else {
            target.nullable_boxed().unwrap_or(target)
        };
        let temporary = self.allocate_temporary();
        let declaration = self.ir.add_expr(IrExpr::Variable {
            index: temporary,
            ty: storage_type,
            init: Some(value),
            named: false,
        });
        let read = self.ir.add_expr(IrExpr::GetValue(temporary));
        let matches = self.ir.add_expr(IrExpr::TypeOp {
            op: IrTypeOp::InstanceOf,
            arg: read,
            type_operand: runtime_target,
        });
        let read = self.ir.add_expr(IrExpr::GetValue(temporary));
        let cast = self.ir.add_expr(IrExpr::TypeOp {
            op: IrTypeOp::Cast,
            arg: read,
            type_operand: runtime_target,
        });
        let null = self.ir.add_expr(IrExpr::Const(crate::ir::IrConst::Null));
        let result = self.ir.add_expr(IrExpr::When {
            branches: vec![(Some(matches), cast), (None, null)],
        });
        Ok(self.ir.add_expr(IrExpr::Block {
            stmts: vec![declaration],
            value: Some(result),
        }))
    }
}
