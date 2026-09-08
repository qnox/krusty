//! Publication of checker types and stable selected results into FIR expression nodes.

use super::*;

impl BodyFirChecker<'_> {
    pub(super) fn expression_origin(
        &mut self,
        expression: ExprId,
    ) -> Result<OriginId, BodyCheckFailure> {
        let span = self
            .file
            .expr_span(expression)
            .ok_or_else(|| self.failure(None, BodyCheckFailureKind::MissingSourceSpan))?;
        Ok(self.origins.source(self.source, span))
    }

    pub(super) fn statement_origin(
        &mut self,
        statement: StmtId,
    ) -> Result<OriginId, BodyCheckFailure> {
        let span = self
            .file
            .stmt_spans
            .get(statement.0 as usize)
            .copied()
            .ok_or_else(|| self.failure(None, BodyCheckFailureKind::MissingSourceSpan))?;
        Ok(self.origins.source(self.source, span))
    }

    pub(super) fn expression_type(
        &self,
        expression: ExprId,
    ) -> Result<ResolvedTy, BodyCheckFailure> {
        // A selected callable's result is the final call-site specialization. The general
        // expression table can still carry the declaration-owned symbolic result from an earlier
        // contextual probe (`invoke<T>(() -> T): T`), while the committed target has already bound
        // it (`T = Nothing`). FIR must publish the selected semantic result; otherwise a callee-local
        // type parameter escapes into a non-generic caller even though resolution is complete.
        if let Some(result) = self
            .info
            .selected_expression_result(expression)
            .and_then(|result| ResolvedTy::new(result).ok())
        {
            return Ok(result);
        }
        let semantic = self.info.semantic_ty(expression);
        ResolvedTy::new(semantic).map_err(|error| {
            self.failure(
                self.file.expr_span(expression),
                BodyCheckFailureKind::UnpublishableType(error),
            )
        })
    }

    pub(super) fn resolved_type(&self, span: Span, ty: Ty) -> Result<ResolvedTy, BodyCheckFailure> {
        ResolvedTy::new(ty).map_err(|error| {
            self.failure(Some(span), BodyCheckFailureKind::UnpublishableType(error))
        })
    }

    pub(super) fn add_expression(
        &mut self,
        source: ExprId,
        kind: FirExprKind,
    ) -> Result<FirExprId, BodyCheckFailure> {
        let origin = self.expression_origin(source)?;
        let ty = self.expression_type(source)?;
        Ok(self.body.add_expr(FirExpr { origin, ty, kind }))
    }

    pub(super) fn add_expression_with_type(
        &mut self,
        source: ExprId,
        ty: ResolvedTy,
        kind: FirExprKind,
    ) -> Result<FirExprId, BodyCheckFailure> {
        let origin = self.expression_origin(source)?;
        Ok(self.body.add_expr(FirExpr { origin, ty, kind }))
    }

    pub(super) fn checked_storage_read(
        &mut self,
        source: ExprId,
        storage_ty: ResolvedTy,
        kind: FirExprKind,
    ) -> Result<FirExprId, BodyCheckFailure> {
        self.checked_storage_read_with_lateinit(source, storage_ty, kind, None)
    }

    pub(super) fn checked_storage_read_with_lateinit(
        &mut self,
        source: ExprId,
        storage_ty: ResolvedTy,
        kind: FirExprKind,
        lateinit_name: Option<&str>,
    ) -> Result<FirExprId, BodyCheckFailure> {
        let origin = self.expression_origin(source)?;
        let mut value = self.body.add_expr(FirExpr {
            origin,
            ty: storage_ty,
            kind,
        });
        if let Some(name) = lateinit_name {
            value = self.body.add_expr(FirExpr {
                origin,
                ty: storage_ty,
                kind: FirExprKind::LateinitRead {
                    value,
                    name: name.into(),
                },
            });
        }
        let result_ty = self.resolved_type(
            self.file
                .expr_span(source)
                .ok_or_else(|| self.failure(None, BodyCheckFailureKind::MissingSourceSpan))?,
            self.info.semantic_ty(source),
        )?;
        // A Unit local/capture/property is already stored as the language singleton. Resolution
        // may expose its classifier view (`kotlin/Unit`) for member selection, but that is not a
        // void-effect-to-value conversion. Preserve the stored read directly so lowering neither
        // manufactures a second singleton nor leaves the original slot value on the stack.
        let stored_unit_view = storage_ty.get().canonical_semantic() == Ty::Unit
            && result_ty.get() == crate::types::stored_value_ty(Ty::Unit);
        let conversion = (!stored_unit_view)
            .then(|| self.selected_type_conversion(storage_ty, result_ty, origin))
            .flatten()
            .or_else(|| {
                (!stored_unit_view
                    && storage_ty != result_ty
                    && storage_ty.get().is_reference()
                    && result_ty.get().is_reference())
                .then_some(FirConversion {
                    origin,
                    kind: FirConversionKind::SmartCast { to: result_ty },
                })
            });
        let Some(conversion) = conversion else {
            return Ok(value);
        };
        Ok(self.body.add_expr(FirExpr {
            origin,
            ty: result_ty,
            kind: FirExprKind::ImplicitConversion { value, conversion },
        }))
    }
}
