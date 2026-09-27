//! Lowering of Kotlin's IEEE-754 `==`/`!=` when a floating operand is nullable.
//!
//! fir2ir builds `ieee754equals` for an equality between two operands of one floating type,
//! whatever their nullability. The null handling is part of that operation, so common IR keeps it
//! as one operation over both operands in source order instead of expanding null tests; a target
//! chooses its realization (kotlinc's JVM backend calls a typed `Intrinsics.areEqual`).

use crate::fir::{FirBinaryOperation, FirExprId};
use crate::ir::{Callee, ExprId, IrBinOp, IrConst, IrExpr, IrIntrinsic};
use crate::types::Ty;

use super::{BodyLowering, FirLoweringFailure};

/// Whether an equality compared as `comparison` is IEEE-754 floating equality.
pub(super) fn is_floating(comparison: Ty) -> bool {
    matches!(comparison, Ty::Float | Ty::Double)
}

impl BodyLowering<'_> {
    /// `lhs == rhs` (or `!=`) of the floating type `operand`, with each operand's own nullability.
    pub(super) fn floating_equality(
        &mut self,
        expression: FirExprId,
        operation: FirBinaryOperation,
        lhs: FirExprId,
        rhs: FirExprId,
        operand: Ty,
    ) -> Result<ExprId, FirLoweringFailure> {
        let lhs = self.expression(lhs)?;
        let rhs = self.expression(rhs)?;
        let equal = self.ir.add_expr(IrExpr::Call {
            callee: Callee::Intrinsic {
                operation: IrIntrinsic::Ieee754Equals { operand },
                ret: Ty::Boolean,
            },
            dispatch_receiver: None,
            args: vec![lhs, rhs],
        });
        // The negation of `!=` is built at the same offsets; the equality marks the line itself.
        let line = self.body.expression_debug_lines(expression).source;
        if line != 0 {
            self.ir.expr_source_lines.insert(equal, line);
        }
        if operation == FirBinaryOperation::Equal {
            return Ok(equal);
        }
        // Boolean negation, as a builtin `Boolean.not()` realizes it.
        let false_value = self.ir.add_expr(IrExpr::Const(IrConst::Boolean(false)));
        Ok(self.ir.add_expr(IrExpr::PrimitiveBinOp {
            op: IrBinOp::Eq,
            lhs: equal,
            rhs: false_value,
        }))
    }
}
