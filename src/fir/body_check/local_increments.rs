//! Increments and decrements of local storage: a lexical local, a captured value, or a local
//! class's captured value, in expression and statement position.

use super::*;

impl BodyFirChecker<'_> {
    /// The updated value of an increment or decrement. A prefix form applies the operator to a read
    /// of `target`; a postfix form applies it to `old_value`, the temporary holding the value it
    /// returns (`val tmp = x; x = tmp.inc(); tmp`), so the storage is read exactly once.
    pub(super) fn increment_value(
        &mut self,
        expression: ExprId,
        target: ExprId,
        old_value: Option<FirExprId>,
        decrement: bool,
    ) -> Result<FirExprId, BodyCheckFailure> {
        let convention = if decrement { "dec" } else { "inc" };
        let operator = self.selected_operator(expression, convention);
        let kind = match (old_value, operator) {
            (None, true) => {
                self.zero_arg_expression_operator_call(expression, convention, target)?
            }
            (Some(operand), true) => {
                self.zero_arg_expression_operator_call_on_value(expression, convention, operand)?
            }
            (old_value, false) => FirExprKind::Unary {
                operation: if decrement {
                    FirUnaryOperation::Decrement
                } else {
                    FirUnaryOperation::Increment
                },
                operand: match old_value {
                    Some(operand) => operand,
                    None => self.expression(target)?,
                },
            },
        };
        self.add_expression(expression, kind)
    }

    pub(super) fn increment_local_expression(
        &mut self,
        expression: ExprId,
        target: ExprId,
        decrement: bool,
        prefix: bool,
    ) -> Result<FirExprKind, BodyCheckFailure> {
        let Expr::Name(name) = self.file.expr(target) else {
            return Err(self.failure(
                self.file.expr_span(expression),
                BodyCheckFailureKind::UnsupportedExpression(ExpressionForm::IncDec),
            ));
        };
        if let Some((depth, delegate)) = self.delegated_binding(name) {
            return self.delegated_inc_dec_expression(
                expression, target, decrement, prefix, depth, delegate,
            );
        }
        let local = self.local(name);
        let captured = if local.is_none() {
            self.outer_values.get(name).copied()
        } else {
            None
        };
        let class_storage = if local.is_none() && captured.is_none() {
            self.class_values.get(name).copied()
        } else {
            None
        };
        if local.is_none() && captured.is_none() && class_storage.is_none() {
            if let Some(kind) =
                self.property_inc_dec_expression(expression, target, decrement, prefix)?
            {
                return Ok(kind);
            }
            return Err(self.failure(
                self.file.expr_span(expression),
                BodyCheckFailureKind::UnknownLocal,
            ));
        }
        if let Some((enclosing_depth, binding)) = captured {
            let cause = self.expression_origin(expression)?;
            self.body.add_capture(FirCapture {
                origin: cause,
                enclosing_depth,
                source: FirCaptureSource::Value(binding.value),
                ty: binding.ty,
                shared_cell: true,
            });
        }
        let resolution = self
            .info
            .resolved_inc_dec
            .get(&IncDecSite::Expression(expression))
            .ok_or_else(|| {
                self.failure(
                    self.file.expr_span(expression),
                    BodyCheckFailureKind::UnsupportedExpression(ExpressionForm::IncDec),
                )
            })?;
        let cause = self.expression_origin(expression)?;
        let mut statements = Vec::new();
        let old_value = if prefix {
            None
        } else {
            let temporary = self.allocate_local();
            let read_kind = match (local, captured, class_storage) {
                (Some(local), _, _) => FirExprKind::ValueRead(local),
                (None, Some((enclosing_depth, binding)), _) => FirExprKind::CapturedValueRead {
                    enclosing_depth,
                    source: binding.value,
                },
                (None, None, Some(binding)) => self.class_storage_read_kind(binding, cause)?,
                (None, None, None) => unreachable!("increment target was checked above"),
            };
            let read = self.body.add_expr(FirExpr {
                origin: cause,
                ty: self.resolved_type(
                    self.file.expr_span(expression).ok_or_else(|| {
                        self.failure(None, BodyCheckFailureKind::MissingSourceSpan)
                    })?,
                    resolution.receiver_ty,
                )?,
                kind: read_kind,
            });
            statements.push(self.body.add_statement(FirStatement {
                origin: cause,
                kind: FirStatementKind::Local {
                    target: temporary,
                    ty: self.resolved_type(
                        self.file.expr_span(expression).ok_or_else(|| {
                            self.failure(None, BodyCheckFailureKind::MissingSourceSpan)
                        })?,
                        resolution.receiver_ty,
                    )?,
                    mutable: false,
                    lateinit: false,
                    deferred: false,
                    initializer: Some(read),
                    conversion: None,
                },
            }));
            Some(temporary)
        };
        let old_value_read = match old_value {
            Some(temporary) => {
                let ty = self.resolved_type(
                    self.file.expr_span(expression).ok_or_else(|| {
                        self.failure(None, BodyCheckFailureKind::MissingSourceSpan)
                    })?,
                    resolution.receiver_ty,
                )?;
                Some(self.body.add_expr(FirExpr {
                    origin: cause,
                    ty,
                    kind: FirExprKind::ValueRead(temporary),
                }))
            }
            None => None,
        };
        let updated = self.increment_value(expression, target, old_value_read, decrement)?;
        let storage_ty = match (local, captured, class_storage) {
            (Some(_), _, _) => {
                self.local_binding(name)
                    .expect("the increment target is a bound local")
                    .ty
            }
            (None, Some((_, binding)), _) => binding.ty,
            (None, None, Some(binding)) => binding.ty,
            (None, None, None) => unreachable!("increment target was checked above"),
        };
        let actual_updated_ty = self
            .body
            .expr(updated)
            .expect("the checked increment result exists")
            .ty;
        let write_conversion = self.selected_type_conversion(actual_updated_ty, storage_ty, cause);
        let write_kind = match (local, captured, class_storage) {
            (Some(local), _, _) => FirExprKind::ValueWrite {
                target: local,
                value: updated,
                conversion: write_conversion,
            },
            (None, Some((enclosing_depth, binding)), _) => FirExprKind::CapturedValueWrite {
                enclosing_depth,
                source: binding.value,
                value: updated,
                conversion: write_conversion,
            },
            (None, None, Some(binding)) => {
                self.class_storage_shared_write_kind(binding, cause, updated, write_conversion)?
            }
            (None, None, None) => unreachable!("increment target was checked above"),
        };
        let write = self.body.add_expr(FirExpr {
            origin: cause,
            ty: ResolvedTy::new(Ty::Unit).expect("Unit is a publishable FIR type"),
            kind: write_kind,
        });
        if old_value.is_some() && local.is_some() {
            self.body.record_postfix_update(write);
        }
        statements.push(self.body.add_statement(FirStatement {
            origin: cause,
            kind: FirStatementKind::Expression(write),
        }));
        let result_kind = match (old_value, local, captured, class_storage) {
            (Some(old_value), _, _, _) => FirExprKind::ValueRead(old_value),
            (None, Some(local), _, _) => FirExprKind::ValueRead(local),
            (None, None, Some((enclosing_depth, binding)), _) => FirExprKind::CapturedValueRead {
                enclosing_depth,
                source: binding.value,
            },
            (None, None, None, Some(binding)) => self.class_storage_read_kind(binding, cause)?,
            (None, None, None, None) => unreachable!("increment target was checked above"),
        };
        let result = self.body.add_expr(FirExpr {
            origin: cause,
            ty: self.expression_type(expression)?,
            kind: result_kind,
        });
        Ok(FirExprKind::Block {
            statements: statements.into_boxed_slice(),
            result: Some(result),
        })
    }

    /// `x++` / `--x` on a local, captured value, local-class value, implicit-receiver property or
    /// backing field in statement position.
    pub(super) fn local_inc_dec_statement(
        &mut self,
        statement: StmtId,
        name: &str,
        dec: bool,
        prefix: bool,
        origin: OriginId,
    ) -> Result<FirStatementId, BodyCheckFailure> {
        if let Some((depth, delegate)) = self.delegated_binding(name) {
            let form = (dec, prefix);
            let update =
                self.delegated_inc_dec_statement(statement, form, depth, delegate, origin)?;
            return Ok(self.body.add_statement(FirStatement {
                origin,
                kind: FirStatementKind::Expression(update),
            }));
        }
        let target = self.local(name);
        let captured = if target.is_none() {
            self.outer_values.get(name).copied()
        } else {
            None
        };
        let class_storage = if target.is_none() && captured.is_none() {
            self.class_values.get(name).copied()
        } else {
            None
        };
        // A member property reached through an implicit receiver has no local/capture slot.
        // Although discarded, `++c` re-reads it; a custom getter makes that observable.
        if target.is_none() && captured.is_none() && class_storage.is_none() {
            // `field++` inside a property accessor. The checker records the backing-field
            // write; read and write both address the enclosing property's storage directly,
            // with no getter/setter in between.
            if matches!(
                self.info.stmt_lowers.get(&statement),
                Some(StmtLowering::BackingFieldWrite { .. })
            ) {
                let property = self.enclosing_property_for_statement(statement)?;
                let span = self
                    .file
                    .stmt_spans
                    .get(statement.0 as usize)
                    .copied()
                    .ok_or_else(|| self.failure(None, BodyCheckFailureKind::MissingSourceSpan))?;
                let resolution = self
                    .info
                    .resolved_inc_dec
                    .get(&IncDecSite::Statement(statement))
                    .copied()
                    .ok_or_else(|| {
                        self.failure(
                            Some(span),
                            BodyCheckFailureKind::UnsupportedStatement(StatementForm::IncDec),
                        )
                    })?;
                let dispatch_receiver =
                    self.backing_field_dispatch_receiver(property, origin, Some(span))?;
                let read = self.body.add_expr(FirExpr {
                    origin,
                    ty: self.resolved_type(span, resolution.receiver_ty)?,
                    kind: FirExprKind::BackingFieldRead {
                        target: property,
                        dispatch_receiver,
                    },
                });
                let convention = if dec { "dec" } else { "inc" };
                let updated_kind = if self
                    .info
                    .resolved_stmt_operator_call(statement, convention)
                    .is_some()
                {
                    self.zero_arg_statement_operator_call_on_value(statement, convention, read)?
                } else {
                    FirExprKind::Unary {
                        operation: if dec {
                            FirUnaryOperation::Decrement
                        } else {
                            FirUnaryOperation::Increment
                        },
                        operand: read,
                    }
                };
                let updated = self.body.add_expr(FirExpr {
                    origin,
                    ty: self.resolved_type(span, resolution.updated_ty)?,
                    kind: updated_kind,
                });
                let write = self.body.add_expr(FirExpr {
                    origin,
                    ty: ResolvedTy::new(Ty::Unit).expect("Unit is a publishable FIR type"),
                    kind: FirExprKind::BackingFieldWrite {
                        target: property,
                        dispatch_receiver,
                        value: updated,
                        conversion: None,
                    },
                });
                return Ok(self.body.add_statement(FirStatement {
                    origin,
                    kind: FirStatementKind::Expression(write),
                }));
            }
            if let Some(kind) = self.implicit_property_inc_dec(statement, dec, prefix)? {
                let write = self.body.add_expr(FirExpr {
                    origin,
                    ty: ResolvedTy::new(Ty::Unit).expect("Unit is a publishable FIR type"),
                    kind,
                });
                return Ok(self.body.add_statement(FirStatement {
                    origin,
                    kind: FirStatementKind::Expression(write),
                }));
            }
            return Err(self.failure(
                self.file.stmt_spans.get(statement.0 as usize).copied(),
                BodyCheckFailureKind::UnknownLocal,
            ));
        }
        let convention = if dec { "dec" } else { "inc" };
        let selected_operator = self
            .info
            .resolved_stmt_operator_call(statement, convention)
            .is_some();
        let resolution = self
            .info
            .resolved_inc_dec
            .get(&IncDecSite::Statement(statement))
            .ok_or_else(|| {
                self.failure(
                    self.file.stmt_spans.get(statement.0 as usize).copied(),
                    BodyCheckFailureKind::UnsupportedStatement(StatementForm::IncDec),
                )
            })?;
        let span = self
            .file
            .stmt_spans
            .get(statement.0 as usize)
            .copied()
            .ok_or_else(|| self.failure(None, BodyCheckFailureKind::MissingSourceSpan))?;
        if let Some((enclosing_depth, binding)) = captured {
            self.body.add_capture(FirCapture {
                origin,
                enclosing_depth,
                source: FirCaptureSource::Value(binding.value),
                ty: binding.ty,
                shared_cell: true,
            });
        }
        let storage_ty = match (target, captured, class_storage) {
            (Some(_), _, _) => {
                self.local_binding(name)
                    .expect("the increment target is a bound local")
                    .ty
            }
            (None, Some((_, binding)), _) => binding.ty,
            (None, None, Some(binding)) => binding.ty,
            (None, None, None) => unreachable!("increment target was checked above"),
        };
        let receiver_ty = self.resolved_type(span, resolution.receiver_ty)?;
        let read_kind = match (target, captured, class_storage) {
            (Some(target), _, _) => FirExprKind::ValueRead(target),
            (None, Some((enclosing_depth, binding)), _) => FirExprKind::CapturedValueRead {
                enclosing_depth,
                source: binding.value,
            },
            (None, None, Some(binding)) => self.class_storage_read_kind(binding, origin)?,
            (None, None, None) => unreachable!("increment target was checked above"),
        };
        let storage_read = self.body.add_expr(FirExpr {
            origin,
            ty: storage_ty,
            kind: read_kind,
        });
        let read_conversion = self
            .selected_type_conversion(storage_ty, receiver_ty, origin)
            .or_else(|| {
                (storage_ty != receiver_ty).then_some(FirConversion {
                    origin,
                    kind: FirConversionKind::SmartCast { to: receiver_ty },
                })
            });
        let read = self.convert_fir_value(storage_read, receiver_ty, origin, read_conversion);
        let updated_kind = if selected_operator {
            self.zero_arg_statement_operator_call_on_value(statement, convention, read)?
        } else {
            FirExprKind::Unary {
                operation: if dec {
                    FirUnaryOperation::Decrement
                } else {
                    FirUnaryOperation::Increment
                },
                operand: read,
            }
        };
        let updated = self.body.add_expr(FirExpr {
            origin,
            ty: self.resolved_type(span, resolution.updated_ty)?,
            kind: updated_kind,
        });
        let updated_ty = self
            .body
            .expr(updated)
            .expect("the checked increment result exists")
            .ty;
        let write_conversion = self.selected_type_conversion(updated_ty, storage_ty, origin);
        let write_kind = match (target, captured, class_storage) {
            (Some(target), _, _) => FirExprKind::ValueWrite {
                target,
                value: updated,
                conversion: write_conversion,
            },
            (None, Some((enclosing_depth, binding)), _) => FirExprKind::CapturedValueWrite {
                enclosing_depth,
                source: binding.value,
                value: updated,
                conversion: write_conversion,
            },
            (None, None, Some(binding)) => {
                self.class_storage_shared_write_kind(binding, origin, updated, write_conversion)?
            }
            (None, None, None) => unreachable!("increment target was checked above"),
        };
        let write = self.body.add_expr(FirExpr {
            origin,
            ty: ResolvedTy::new(Ty::Unit).expect("Unit is a publishable FIR type"),
            kind: write_kind,
        });
        Ok(self.body.add_statement(FirStatement {
            origin,
            kind: FirStatementKind::Expression(write),
        }))
    }
}
