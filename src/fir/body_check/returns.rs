//! Checked `return` values for the body under construction.
//!
//! A declaration body, a lambda body, and a non-local inline return each name a different target.
//! This module materializes the value that leaves the body this checker owns.

use super::*;

impl BodyFirChecker<'_> {
    pub(super) fn default_return_target(&self) -> ReturnTarget {
        self.lambda_return_source
            .map_or(ReturnTarget::Function, ReturnTarget::Lambda)
    }

    pub(super) fn return_value(
        &mut self,
        source: ExprId,
        target: Option<&ReturnTarget>,
    ) -> Result<FirExprId, BodyCheckFailure> {
        let target_type = match target {
            Some(ReturnTarget::Lambda(lambda)) if Some(*lambda) == self.lambda_return_source => {
                self.body.result_type()
            }
            Some(ReturnTarget::Function) if self.lambda_return_source.is_some() => self
                .index
                .signature(DeclarationId::from_raw(self.body.owner().raw()))
                .map(|signature| signature.result),
            Some(ReturnTarget::Function) => self
                .body
                .result_type()
                .or_else(|| {
                    self.index
                        .signature(DeclarationId::from_raw(self.body.owner().raw()))
                        .map(|signature| signature.result)
                })
                .or_else(|| ResolvedTy::new(self.info.semantic_ty(source)).ok()),
            Some(ReturnTarget::Lambda(lambda)) => {
                let Ty::Fun(signature) = self.info.semantic_ty(*lambda).non_null() else {
                    return Err(self.failure(
                        self.file.expr_span(source),
                        BodyCheckFailureKind::UnsupportedExpression(ExpressionForm::Return),
                    ));
                };
                ResolvedTy::new(signature.ret).ok()
            }
            None => ResolvedTy::new(self.info.semantic_ty(source)).ok(),
        }
        .ok_or_else(|| {
            self.failure(
                self.file.expr_span(source),
                BodyCheckFailureKind::UnsupportedExpression(ExpressionForm::Return),
            )
        })?;
        let cause = self.expression_origin(source)?;
        let actual_type = self.expression_type(source)?;
        let value = self.expression(source)?;
        let conversion = self
            .selected_value_conversion(source, value, target_type, cause)?
            .filter(|conversion| {
                // A named function's Unit return is a statement boundary. Its effect must remain
                // void here; callable realization materializes Unit only for lambdas/adapters that
                // actually return it as a value.
                !(matches!(target, Some(ReturnTarget::Function))
                    && actual_type.get().canonical_semantic() == Ty::Unit
                    && target_type.get().canonical_semantic() == Ty::Unit
                    && matches!(conversion.kind, FirConversionKind::CoerceToUnit))
            });
        let Some(conversion) = conversion else {
            return Ok(value);
        };
        Ok(self.body.add_expr(FirExpr {
            origin: cause,
            ty: target_type,
            kind: FirExprKind::ImplicitConversion { value, conversion },
        }))
    }

    /// A valueless return from a lambda still has Kotlin's `Unit` value. Preserve it in checked FIR
    /// whenever the selected lambda result is `Unit` or `Unit?`; an ordinary named Unit function
    /// keeps a valueless return because its callable boundary is statement-valued.
    pub(super) fn valueless_lambda_return_value(
        &mut self,
        target: Option<&ReturnTarget>,
        cause: OriginId,
    ) -> Result<Option<FirExprId>, BodyCheckFailure> {
        let Some(ReturnTarget::Lambda(lambda)) = target else {
            return Ok(None);
        };
        let target_type = if Some(*lambda) == self.lambda_return_source {
            self.body.result_type()
        } else {
            let Ty::Fun(signature) = self.info.semantic_ty(*lambda).non_null() else {
                return Err(self.failure(
                    self.file.expr_span(*lambda),
                    BodyCheckFailureKind::UnsupportedExpression(ExpressionForm::Return),
                ));
            };
            ResolvedTy::new(signature.ret).ok()
        }
        .ok_or_else(|| {
            self.failure(
                self.file.expr_span(*lambda),
                BodyCheckFailureKind::UnsupportedExpression(ExpressionForm::Return),
            )
        })?;
        if target_type.get().non_null() != Ty::Unit {
            return Ok(None);
        }
        let value_origin = self
            .origins
            .synthetic(cause, SyntheticOriginKind::GeneratedControlFlow);
        let unit_type = ResolvedTy::new(Ty::Unit).expect("Unit is a publishable FIR type");
        let unit = self.body.add_expr(FirExpr {
            origin: value_origin,
            ty: unit_type,
            kind: FirExprKind::SingletonValue {
                classifier: crate::types::type_name("kotlin/Unit"),
            },
        });
        if target_type == unit_type {
            return Ok(Some(unit));
        }
        let conversion_origin = self
            .origins
            .synthetic(cause, SyntheticOriginKind::ImplicitConversion);
        Ok(Some(self.body.add_expr(FirExpr {
            origin: conversion_origin,
            ty: target_type,
            kind: FirExprKind::ImplicitConversion {
                value: unit,
                conversion: FirConversion {
                    origin: conversion_origin,
                    kind: FirConversionKind::NullabilityWidening { to: target_type },
                },
            },
        })))
    }

    /// Whether a checked `return` leaves the body this checker is constructing.
    ///
    /// A declaration body owns `ReturnTarget::Function`. A lambda or anonymous-function body owns
    /// `ReturnTarget::Lambda` of its own expression: `fun (x: T): R { return e }` returns from the
    /// anonymous function, and `return@label` returns from the labeled lambda. Any other lambda
    /// target is a NON-LOCAL return through an enclosing inline frame, which is a separate checked
    /// form.
    pub(super) fn return_target_depth(&self, target: Option<&ReturnTarget>) -> Option<u32> {
        match target {
            Some(ReturnTarget::Function) => Some(self.function_return_depth),
            Some(ReturnTarget::Lambda(source)) if Some(*source) == self.lambda_return_source => {
                Some(0)
            }
            Some(ReturnTarget::Lambda(source)) => {
                self.outer_lambda_return_depths.get(source).copied()
            }
            None => None,
        }
    }
}
