use crate::fir::{FirExprId, FirRangeOperation, OriginId};
use crate::ir::{ExprId, IrBinOp, IrCheckedOperation, IrConst, IrExpr, IrTypeOp};
use crate::types::{stored_value_ty, Ty};

use super::{BodyLowering, FirLoweringFailure};

pub(super) struct InRangeLowering {
    operation: FirRangeOperation,
    comparison_ty: Ty,
    unsigned_compare: Option<crate::fir::FirRuntimeFunction>,
    operands: [FirExprId; 3],
    negated: bool,
    origin: OriginId,
}

impl InRangeLowering {
    pub(super) fn new(
        operation: FirRangeOperation,
        comparison_ty: Ty,
        unsigned_compare: Option<crate::fir::FirRuntimeFunction>,
        operands: [FirExprId; 3],
        negated: bool,
        origin: OriginId,
    ) -> Self {
        Self {
            operation,
            comparison_ty,
            unsigned_compare,
            operands,
            negated,
            origin,
        }
    }
}

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
        Ok(self.ir.add_expr(IrExpr::Checked(Box::new(
            IrCheckedOperation::RangeConstruction {
                operation,
                start,
                start_type,
                end,
                end_type,
                result,
            },
        ))))
    }

    pub(super) fn in_range_expression(
        &mut self,
        range: InRangeLowering,
    ) -> Result<ExprId, FirLoweringFailure> {
        let InRangeLowering {
            operation,
            comparison_ty,
            unsigned_compare,
            operands: [value, start, end],
            negated,
            origin,
        } = range;
        if !matches!(
            comparison_ty,
            Ty::Int | Ty::Long | Ty::Char | Ty::UInt | Ty::ULong | Ty::Float | Ty::Double
        ) {
            return Err(FirLoweringFailure::InvalidRangeCounter {
                origin,
                ty: comparison_ty,
            });
        }
        let value_ty = self.expression_ty(value)?;
        if matches!(comparison_ty, Ty::UInt | Ty::ULong)
            || !(value_ty.is_reference() && comparison_ty.is_jvm_scalar())
        {
            // The backend realizes membership in a range literal as kotlinc's
            // `RangeContainsLowering` does, from the operands as written.
            let start = self.expression(start)?;
            let end = self.expression(end)?;
            let value = self.expression(value)?;
            return Ok(self.ir.add_expr(IrExpr::Checked(Box::new(
                IrCheckedOperation::RangeContains {
                    operation,
                    value,
                    start,
                    end,
                    negated,
                    counter: comparison_ty,
                    unsigned_compare: unsigned_compare.map(|function| {
                        crate::ir::IrRuntimeFunction {
                            function: function.function,
                            parameters: function.parameters.to_vec(),
                            result: function.result,
                        }
                    }),
                },
            ))));
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
        // A reference value over a primitive range is an element only when it is a boxed one.
        let value = self.expression(value)?;
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
        let contained = self.range_membership(
            operation,
            comparison_ty,
            negated,
            start_slot,
            end_slot,
            scalar_slot,
        );
        let matched = self.ir.add_expr(IrExpr::Block {
            stmts: vec![scalar],
            value: Some(contained),
        });
        let mismatch = self.ir.add_expr(IrExpr::Const(IrConst::Boolean(negated)));
        let result = self.ir.add_expr(IrExpr::When {
            branches: vec![(Some(matches), matched), (None, mismatch)],
        });
        self.ir.logical_types.insert(matched, Ty::Boolean);
        self.ir.logical_types.insert(result, Ty::Boolean);
        Ok(self.ir.add_expr(IrExpr::Block {
            stmts: declarations,
            value: Some(result),
        }))
    }

    /// `value in receiver.indices`: membership in `0 until size`, which the backend realizes as
    /// kotlinc's `RangeContainsLowering` does, from the size read the `indices` loop also reads.
    pub(super) fn indices_contains_expression(
        &mut self,
        value: FirExprId,
        indices: &crate::fir::FirIndices,
        comparison: Ty,
    ) -> Result<ExprId, FirLoweringFailure> {
        let start = self.ir.add_expr(IrExpr::Const(IrConst::Int(0)));
        let end = self.indices_size(indices)?;
        self.ir.logical_types.insert(end, Ty::Int);
        let value = self.expression(value)?;
        Ok(self.ir.add_expr(IrExpr::Checked(Box::new(
            IrCheckedOperation::RangeContains {
                operation: FirRangeOperation::Until,
                value,
                start,
                end,
                negated: false,
                counter: comparison,
                unsigned_compare: None,
            },
        ))))
    }

    fn range_membership(
        &mut self,
        operation: FirRangeOperation,
        comparison_ty: Ty,
        negated: bool,
        start_slot: u32,
        end_slot: u32,
        value_slot: u32,
    ) -> ExprId {
        if negated && matches!(comparison_ty, Ty::Float | Ty::Double) {
            // IEEE floating order is partial: neither `<` nor `>` holds for NaN. Negate the
            // positive conjunction instead of applying the integral De Morgan rewrite.
            let contained = self.range_membership(
                operation,
                comparison_ty,
                false,
                start_slot,
                end_slot,
                value_slot,
            );
            return self.ir.add_negation(contained);
        }
        let (low, high, high_strict) = match operation {
            FirRangeOperation::Through => (start_slot, end_slot, false),
            FirRangeOperation::OpenEnd | FirRangeOperation::Until => (start_slot, end_slot, true),
            FirRangeOperation::DownTo => (end_slot, start_slot, false),
        };
        if negated {
            let below = self.comparison(IrBinOp::Lt, comparison_ty, value_slot, low);
            let above = self.comparison(
                if high_strict {
                    IrBinOp::Ge
                } else {
                    IrBinOp::Gt
                },
                comparison_ty,
                value_slot,
                high,
            );
            self.ir.add_expr(IrExpr::PrimitiveBinOp {
                op: IrBinOp::Or,
                lhs: below,
                rhs: above,
            })
        } else {
            let above_low = self.comparison(IrBinOp::Le, comparison_ty, low, value_slot);
            let below_high = self.comparison(
                if high_strict {
                    IrBinOp::Lt
                } else {
                    IrBinOp::Le
                },
                comparison_ty,
                value_slot,
                high,
            );
            self.ir.add_expr(IrExpr::PrimitiveBinOp {
                op: IrBinOp::And,
                lhs: above_low,
                rhs: below_high,
            })
        }
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

    fn comparison(&mut self, operation: IrBinOp, ty: Ty, lhs: u32, rhs: u32) -> ExprId {
        let lhs = self.ir.add_expr(IrExpr::GetValue(lhs));
        let rhs = self.ir.add_expr(IrExpr::GetValue(rhs));
        // These reads are synthesized from temporaries declared above, rather than lowered from
        // source FIR expressions. Preserve the selected range-comparison type on each operand so
        // targets choose only its representation.
        self.ir.logical_types.insert(lhs, ty);
        self.ir.logical_types.insert(rhs, ty);
        self.ir.add_expr(IrExpr::PrimitiveBinOp {
            op: operation,
            lhs,
            rhs,
        })
    }
}
