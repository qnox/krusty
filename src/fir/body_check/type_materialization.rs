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
        if let Ok(ty) = ResolvedTy::new(semantic) {
            return Ok(ty);
        }
        // Local inferred callables are finalized after the active Pass-2 file is checked. The
        // checker's expression table can therefore still carry its local collection marker even
        // though the selected call already has a stable declaration and the active-file index now
        // owns the final type. Consume that exact selected identity; never repeat lookup here.
        let declaration = self
            .info
            .resolved_calls
            .get(&expression)
            .and_then(|call| match call {
                ResolvedCall::Member(selected) => selected.member.stable_declaration,
                ResolvedCall::TopLevel(selected) => selected.stable_declaration,
                ResolvedCall::Companion(selected) => selected.stable_declaration,
                ResolvedCall::Extension(selected) => selected.stable_declaration,
                ResolvedCall::MemberExtension {
                    stable_declaration, ..
                } => *stable_declaration,
                ResolvedCall::LocalFunction(selected) => selected.sig.stable_declaration,
            });
        if let Some(result) = declaration
            .and_then(|declaration| self.index.signature(declaration))
            .map(|signature| signature.result)
        {
            return Ok(result);
        }
        // A body-local inferred property may have been finalized after the legacy checker recorded
        // this access. Its selected property identity is stable even when the transient expression
        // table still contains the earlier `Error` marker. Consume the published signature exactly
        // as the callable path above does; FIR must not preserve or reinterpret that marker.
        let property_declaration = match self.info.expr_lowers.get(&expression) {
            Some(ExprLowering::MemberPropertyRead {
                stable_declaration, ..
            })
            | Some(ExprLowering::MemberExtensionPropertyRead {
                stable_declaration, ..
            }) => *stable_declaration,
            Some(ExprLowering::TopLevelPropertyGet(access))
            | Some(ExprLowering::ExtensionPropertyGet { access }) => {
                access.property.stable_declaration
            }
            _ => None,
        };
        if let Some(result) = property_declaration
            .and_then(|declaration| self.index.signature(declaration))
            .map(|signature| signature.result)
        {
            return Ok(result);
        }
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
        let semantic = self.info.semantic_ty(source);
        let stable = match &kind {
            FirExprKind::Call(call) => match &call.target {
                FirCallTarget::Module(callable) => self
                    .index
                    .callable(*callable)
                    .and_then(|callable| self.index.signature(callable.declaration))
                    .and_then(|signature| {
                        let bindings = call
                            .substitutions
                            .iter()
                            .filter_map(|substitution| {
                                let FirTypeParameterRef::Module(parameter) = substitution.parameter
                                else {
                                    return None;
                                };
                                self.index
                                    .type_parameter_semantic_name(parameter)
                                    .map(|name| (name.to_owned(), substitution.value.get()))
                            })
                            .collect::<std::collections::HashMap<_, _>>();
                        ResolvedTy::new(crate::types::ty_subst_keep_unbound(
                            signature.result.get(),
                            &bindings,
                        ))
                        .ok()
                    }),
                FirCallTarget::External { result, .. }
                | FirCallTarget::Intrinsic { result, .. }
                | FirCallTarget::Classifier { result, .. }
                | FirCallTarget::Super { result, .. } => Some(*result),
            },
            FirExprKind::PropertyRead {
                target: FirPropertyTarget::Module(property),
                substitutions,
                ..
            } => self
                .index
                .property_declaration(*property)
                .and_then(|declaration| self.index.signature(declaration))
                .and_then(|signature| {
                    let bindings = substitutions
                        .iter()
                        .filter_map(|substitution| {
                            let FirTypeParameterRef::Module(parameter) = substitution.parameter
                            else {
                                return None;
                            };
                            self.index
                                .type_parameter_semantic_name(parameter)
                                .map(|name| (name.to_owned(), substitution.value.get()))
                        })
                        .collect::<crate::symbol_resolver::GSigBinds>();
                    ResolvedTy::new(crate::types::ty_subst_keep_unbound(
                        signature.result.get(),
                        &bindings,
                    ))
                    .ok()
                }),
            FirExprKind::PropertyRead {
                target: FirPropertyTarget::External { result, .. },
                ..
            } => Some(*result),
            FirExprKind::Block {
                result: Some(result),
                ..
            } => self.body.expr(*result).map(|result| result.ty),
            _ => None,
        };
        let stable_more_concrete = semantic.mentions_ty_param()
            && stable.is_some_and(|stable| !stable.get().mentions_ty_param());
        let ty = if semantic.mentions_error() || stable_more_concrete {
            crate::trace_compiler!(
                "fir",
                "expression publication uses stable type expression={source:?} span={:?} ast={:?} semantic={semantic:?} kind={:?}",
                self.file.expr_span(source),
                self.file.expr(source),
                std::mem::discriminant(&kind),
            );
            stable.ok_or_else(|| {
                self.failure(
                    self.file.expr_span(source),
                    BodyCheckFailureKind::UnpublishableType(crate::fir::UnpublishableType::Error),
                )
            })?
        } else {
            self.expression_type(source)?
        };
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
        // The stable binding is authoritative for a lexical read. During the two-pass cutover the
        // legacy checker can leave an `Error` cache entry for a nested function-type spelling even
        // though Pass 1 has already published the complete parameter type. Do not copy that stale
        // cache marker into FIR; use the selected binding type. Pending types remain hard errors.
        let semantic = self.info.semantic_ty(source);
        let result_ty = if semantic.mentions_error() {
            storage_ty
        } else {
            self.resolved_type(
                self.file
                    .expr_span(source)
                    .ok_or_else(|| self.failure(None, BodyCheckFailureKind::MissingSourceSpan))?,
                semantic,
            )?
        };
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
