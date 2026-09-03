use crate::fir::{FirExprId, FirRangeOperation, OriginId};
use crate::ir::{ExprId, IrBinOp, IrCheckedOperation, IrConst, IrExpr, IrTypeOp};
use crate::types::{stored_value_ty, Ty};

use super::{BodyLowering, FirLoweringFailure};

impl BodyLowering<'_> {
    pub(super) fn range_expression(
        &mut self,
        operation: FirRangeOperation,
        start: FirExprId,
        start_type: Ty,
        end: FirExprId,
        end_type: Ty,
        result: Ty,
    ) -> Result<ExprId, FirLoweringFailure> {
        let start = self.expression(start)?;
        let end = self.expression(end)?;
        Ok(self
            .ir
            .add_expr(IrExpr::Checked(IrCheckedOperation::RangeConstruction {
                operation,
                start,
                start_type,
                end,
                end_type,
                result,
            })))
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn in_range_expression(
        &mut self,
        operation: FirRangeOperation,
        comparison_ty: Ty,
        value: FirExprId,
        start: FirExprId,
        end: FirExprId,
        negated: bool,
        origin: OriginId,
    ) -> Result<ExprId, FirLoweringFailure> {
        if !matches!(
            comparison_ty,
            Ty::Int | Ty::Long | Ty::Char | Ty::UInt | Ty::ULong | Ty::Float | Ty::Double
        ) {
            return Err(FirLoweringFailure::InvalidRangeCounter {
                origin,
                ty: comparison_ty,
            });
        }
        if matches!(comparison_ty, Ty::UInt | Ty::ULong) {
            let value = self.expression(value)?;
            let start = self.expression(start)?;
            let end = self.expression(end)?;
            return Ok(self
                .ir
                .add_expr(IrExpr::Checked(IrCheckedOperation::RangeContains {
                    operation,
                    value,
                    start,
                    end,
                    negated,
                    counter: comparison_ty,
                })));
        }
        let mut declarations = Vec::with_capacity(3);
        let start = self.expression(start)?;
        let start = self.coerce_range_operand(start, comparison_ty);
        let start_slot = self.allocate_temporary();
        declarations.push(self.ir.add_expr(IrExpr::Variable {
            index: start_slot,
            ty: comparison_ty,
            init: Some(start),
            named: false,
        }));
        let end = self.expression(end)?;
        let end = self.coerce_range_operand(end, comparison_ty);
        let end_slot = self.allocate_temporary();
        declarations.push(self.ir.add_expr(IrExpr::Variable {
            index: end_slot,
            ty: comparison_ty,
            init: Some(end),
            named: false,
        }));
        let value_ty = self.expression_ty(value)?;
        let value = self.expression(value)?;
        if value_ty.is_reference() && comparison_ty.is_jvm_scalar() {
            let value_slot = self.allocate_temporary();
            declarations.push(self.ir.add_expr(IrExpr::Variable {
                index: value_slot,
                ty: stored_value_ty(value_ty),
                init: Some(value),
                named: false,
            }));
            let value_read = self.ir.add_expr(IrExpr::GetValue(value_slot));
            let matches = self.ir.add_expr(IrExpr::TypeOp {
                op: IrTypeOp::InstanceOf,
                arg: value_read,
                type_operand: comparison_ty,
            });
            let value_read = self.ir.add_expr(IrExpr::GetValue(value_slot));
            let value = self.coerce_range_operand(value_read, comparison_ty);
            let scalar_slot = self.allocate_temporary();
            let scalar = self.ir.add_expr(IrExpr::Variable {
                index: scalar_slot,
                ty: comparison_ty,
                init: Some(value),
                named: false,
            });
            let contained =
                self.range_membership(operation, negated, start_slot, end_slot, scalar_slot);
            let matched = self.ir.add_expr(IrExpr::Block {
                stmts: vec![scalar],
                value: Some(contained),
            });
            let mismatch = self.ir.add_expr(IrExpr::Const(IrConst::Boolean(negated)));
            let result = self.ir.add_expr(IrExpr::When {
                branches: vec![(Some(matches), matched), (None, mismatch)],
            });
            return Ok(self.ir.add_expr(IrExpr::Block {
                stmts: declarations,
                value: Some(result),
            }));
        }
        let value = self.coerce_range_operand(value, comparison_ty);
        let value_slot = self.allocate_temporary();
        declarations.push(self.ir.add_expr(IrExpr::Variable {
            index: value_slot,
            ty: comparison_ty,
            init: Some(value),
            named: false,
        }));
        let result = self.range_membership(operation, negated, start_slot, end_slot, value_slot);
        Ok(self.ir.add_expr(IrExpr::Block {
            stmts: declarations,
            value: Some(result),
        }))
    }

    fn range_membership(
        &mut self,
        operation: FirRangeOperation,
        negated: bool,
        start_slot: u32,
        end_slot: u32,
        value_slot: u32,
    ) -> ExprId {
        let (low, high, high_strict) = match operation {
            FirRangeOperation::Through => (start_slot, end_slot, false),
            FirRangeOperation::OpenEnd | FirRangeOperation::Until => (start_slot, end_slot, true),
            FirRangeOperation::DownTo => (end_slot, start_slot, false),
        };
        let result = if negated {
            let below = self.comparison(IrBinOp::Lt, value_slot, low);
            let above = self.comparison(
                if high_strict {
                    IrBinOp::Ge
                } else {
                    IrBinOp::Gt
                },
                value_slot,
                high,
            );
            self.ir.add_expr(IrExpr::PrimitiveBinOp {
                op: IrBinOp::Or,
                lhs: below,
                rhs: above,
            })
        } else {
            let above_low = self.comparison(IrBinOp::Le, low, value_slot);
            let below_high = self.comparison(
                if high_strict {
                    IrBinOp::Lt
                } else {
                    IrBinOp::Le
                },
                value_slot,
                high,
            );
            self.ir.add_expr(IrExpr::PrimitiveBinOp {
                op: IrBinOp::And,
                lhs: above_low,
                rhs: below_high,
            })
        };
        result
    }

    fn expression_ty(&self, expression: FirExprId) -> Result<Ty, FirLoweringFailure> {
        self.body
            .expr(expression)
            .map(|expression| expression.ty.get())
            .ok_or(FirLoweringFailure::MissingExpression(expression))
    }

    fn coerce_range_operand(&mut self, expression: ExprId, target: Ty) -> ExprId {
        self.ir.add_expr(IrExpr::TypeOp {
            op: IrTypeOp::ImplicitCoercion,
            arg: expression,
            type_operand: target,
        })
    }

    fn comparison(&mut self, operation: IrBinOp, lhs: u32, rhs: u32) -> ExprId {
        let lhs = self.ir.add_expr(IrExpr::GetValue(lhs));
        let rhs = self.ir.add_expr(IrExpr::GetValue(rhs));
        self.ir.add_expr(IrExpr::PrimitiveBinOp {
            op: operation,
            lhs,
            rhs,
        })
    }
}
