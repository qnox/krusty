//! Checked FIR for Kotlin's built-in binary operators on primitive values: the operand types an
//! operation compares or computes at, and the conversions that bring each operand there.

use super::*;

/// Semantic operand type used to select Kotlin's built-in equality shape. A bare type parameter
/// contributes its declared upper bound, including that bound's nullability. This is deliberately
/// separate from range selection: flexible platform nullability remains part of equality.
fn equality_operand_type(ty: Ty) -> Ty {
    match ty.canonical_semantic() {
        Ty::TyParam(_, bound) => equality_operand_type(*bound),
        ty => ty,
    }
}

/// One operand of a built-in binary operation: its source expression, its checked type, and the
/// type the operation consumes it at when that differs from the checked type.
pub(super) struct BinaryOperand {
    pub(super) source: ExprId,
    pub(super) ty: ResolvedTy,
    pub(super) target: Option<ResolvedTy>,
}

impl BodyFirChecker<'_> {
    pub(super) fn checked_equality_expression(
        &mut self,
        expression: ExprId,
        source_operation: BinOp,
        lhs: ExprId,
        rhs: ExprId,
    ) -> Result<FirExprKind, BodyCheckFailure> {
        debug_assert!(matches!(source_operation, BinOp::Eq | BinOp::Ne));
        let lhs_ty = equality_operand_type(self.info.semantic_ty(lhs));
        let rhs_ty = equality_operand_type(self.info.semantic_ty(rhs));
        crate::trace_compiler!(
            "fir",
            "checked equality expression={expression:?} lhs={lhs_ty:?} rhs={rhs_ty:?}"
        );
        let operation = if source_operation == BinOp::Eq {
            FirBinaryOperation::Equal
        } else {
            FirBinaryOperation::NotEqual
        };
        let nullable_numeric = lhs_ty
            .nullable_primitive()
            .zip(rhs_ty.nullable_primitive())
            .and_then(|(lhs_primitive, rhs_primitive)| {
                Ty::promote(lhs_primitive, rhs_primitive)
                    .map(|comparison| (lhs_primitive, rhs_primitive, comparison))
            });
        if let Some((lhs_primitive, rhs_primitive, comparison)) = nullable_numeric {
            let resolved = |checker: &Self, source: ExprId, ty: Ty| {
                checker.resolved_type(
                    checker.file.expr_span(source).ok_or_else(|| {
                        checker.failure(None, BodyCheckFailureKind::MissingSourceSpan)
                    })?,
                    ty,
                )
            };
            return Ok(FirExprKind::NullableNumericComparison {
                operation,
                lhs: self.expression(lhs)?,
                rhs: self.expression(rhs)?,
                lhs_primitive: resolved(self, lhs, lhs_primitive)?,
                rhs_primitive: resolved(self, rhs, rhs_primitive)?,
                comparison: resolved(self, expression, comparison)?,
            });
        }

        let operands = lhs_ty
            .nullable_primitive()
            .filter(|primitive| *primitive == rhs_ty)
            .map(|primitive| (lhs, rhs, primitive, true))
            .or_else(|| {
                rhs_ty
                    .nullable_primitive()
                    .filter(|primitive| *primitive == lhs_ty)
                    .map(|primitive| (rhs, lhs, primitive, false))
            });
        let Some((nullable, primitive, primitive_ty, nullable_first)) = operands else {
            return self.builtin_binary_expression(expression, source_operation, lhs, rhs);
        };
        Ok(FirExprKind::NullablePrimitiveComparison {
            operation,
            nullable: self.expression(nullable)?,
            primitive: self.expression(primitive)?,
            primitive_ty: self.resolved_type(
                self.file
                    .expr_span(primitive)
                    .ok_or_else(|| self.failure(None, BodyCheckFailureKind::MissingSourceSpan))?,
                primitive_ty,
            )?,
            nullable_first,
        })
    }

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
        let lhs = operand(self, lhs)?;
        let rhs = operand(self, rhs)?;
        if matches!(
            operation,
            FirBinaryOperation::Equal | FirBinaryOperation::NotEqual
        ) {
            let lhs_ty = self
                .body
                .expr(lhs)
                .expect("the equality operand was just built")
                .ty
                .get();
            let rhs_ty = self
                .body
                .expr(rhs)
                .expect("the equality operand was just built")
                .ty
                .get();
            return Ok(FirExprKind::Equality {
                operation,
                mode: source_equality_mode(lhs_ty, rhs_ty),
                lhs,
                rhs,
            });
        }
        Ok(FirExprKind::Binary {
            operation,
            lhs,
            rhs,
        })
    }
}

/// The equality mode of the types written at the comparison, before inline substitution.
fn source_equality_mode(lhs: Ty, rhs: Ty) -> crate::equality::EqualityMode {
    use crate::equality::EqualityMode;
    let left = lhs.canonical_semantic().scalar_value_repr();
    let right = rhs.canonical_semantic().scalar_value_repr();
    match (left, right) {
        (Some(left), Some(right)) if matches!(left, Ty::Float | Ty::Double) && left == right => {
            EqualityMode::Ieee754
        }
        (Some(_), Some(_)) => EqualityMode::Primitive,
        _ => EqualityMode::Structural,
    }
}
