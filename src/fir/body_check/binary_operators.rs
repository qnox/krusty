//! Checked FIR for Kotlin's built-in binary operators on primitive values: the operand types an
//! operation compares or computes at, and the conversions that bring each operand there.

use super::*;

/// One operand of a built-in binary operation: its source expression, its checked type, and the
/// type the operation consumes it at when that differs from the checked type.
pub(super) struct BinaryOperand {
    pub(super) source: ExprId,
    pub(super) ty: ResolvedTy,
    pub(super) target: Option<ResolvedTy>,
}

impl BodyFirChecker<'_> {
    pub(super) fn builtin_binary_expression(
        &mut self,
        expression: ExprId,
        operation: BinOp,
        lhs: ExprId,
        rhs: ExprId,
    ) -> Result<FirExprKind, BodyCheckFailure> {
        let lhs_type = self.expression_type(lhs)?;
        let rhs_type = self.expression_type(rhs)?;
        // Equality between two different primitive numbers (reachable only through smart casts,
        // `a is Int && b is Long && a == b`) compares at their common numeric type, as arithmetic
        // and ordering do; equality of one primitive type compares that type directly.
        let promotes_operands = match operation {
            BinOp::Add
            | BinOp::Sub
            | BinOp::Mul
            | BinOp::Div
            | BinOp::Rem
            | BinOp::Lt
            | BinOp::Le
            | BinOp::Gt
            | BinOp::Ge => true,
            BinOp::Eq | BinOp::Ne => {
                lhs_type.get().canonical_semantic() != rhs_type.get().canonical_semantic()
            }
            BinOp::And | BinOp::Or | BinOp::RefEq | BinOp::RefNe => false,
        };
        let operation = match operation {
            BinOp::Add => FirBinaryOperation::Add,
            BinOp::Sub => FirBinaryOperation::Subtract,
            BinOp::Mul => FirBinaryOperation::Multiply,
            BinOp::Div => FirBinaryOperation::Divide,
            BinOp::Rem => FirBinaryOperation::Remainder,
            BinOp::Eq => FirBinaryOperation::Equal,
            BinOp::Ne => FirBinaryOperation::NotEqual,
            BinOp::Lt => FirBinaryOperation::Less,
            BinOp::Le => FirBinaryOperation::LessOrEqual,
            BinOp::Gt => FirBinaryOperation::Greater,
            BinOp::Ge => FirBinaryOperation::GreaterOrEqual,
            BinOp::And => FirBinaryOperation::BooleanAnd,
            BinOp::Or => FirBinaryOperation::BooleanOr,
            BinOp::RefEq => FirBinaryOperation::ReferentialEqual,
            BinOp::RefNe => FirBinaryOperation::ReferentialNotEqual,
        };
        let promoted = promotes_operands
            .then(|| Ty::promote(lhs_type.get(), rhs_type.get()))
            .flatten()
            .map(|ty| {
                self.resolved_type(
                    self.file
                        .expr_span(expression)
                        .expect("a checked binary expression has a source span"),
                    ty,
                )
            })
            .transpose()?;
        self.checked_binary_expression_at_targets(
            operation,
            BinaryOperand {
                source: lhs,
                ty: lhs_type,
                target: promoted,
            },
            BinaryOperand {
                source: rhs,
                ty: rhs_type,
                target: promoted,
            },
        )
    }

    pub(super) fn checked_binary_expression_at_targets(
        &mut self,
        operation: FirBinaryOperation,
        lhs: BinaryOperand,
        rhs: BinaryOperand,
    ) -> Result<FirExprKind, BodyCheckFailure> {
        let operand =
            |checker: &mut Self, operand: BinaryOperand| -> Result<FirExprId, BodyCheckFailure> {
                let BinaryOperand {
                    source,
                    ty: actual,
                    target,
                } = operand;
                let value = checker.expression(source)?;
                let cause = checker.expression_origin(source)?;
                // Binary operands are value positions. A Unit-returning call is effect-only at its
                // callable boundary, so publish the language-level Unit materialization explicitly;
                // common lowering must not infer that requirement from the eventual operation.
                let unit_value = actual.get().canonical_semantic() == Ty::Unit;
                let Some(target) = target.or(unit_value.then_some(actual)) else {
                    return Ok(value);
                };
                // The expression's semantic type may already be the smart-cast target while its source
                // value still occupies a nullable/platform reference slot. Consume the resolver's exact
                // selected boundary as call arguments do; comparing only `actual == target` loses the
                // required unbox on primitive operators inside a non-null branch.
                let conversion =
                    checker.selected_value_conversion_from(source, value, actual, target, cause)?;
                Ok(checker.convert_fir_value(value, target, cause, conversion))
            };
        Ok(FirExprKind::Binary {
            operation,
            lhs: operand(self, lhs)?,
            rhs: operand(self, rhs)?,
        })
    }
}
