//! Checked FIR for an unparenthesized safe index.
//!
//! The source receiver is one compiler-generated local. `get`, `set`, and `inc`/`dec` run only
//! after that local is non-null, on the member selected for that branch. Index operands reused by
//! an update are bound inside the branch, so a null receiver does not evaluate them.

use super::*;

pub(super) struct SafeIndexIncDec<'a> {
    pub(super) expression: ExprId,
    pub(super) receiver: ExprId,
    pub(super) access: ExprId,
    pub(super) element: ExprId,
    pub(super) indices: &'a [ExprId],
    pub(super) updated: ExprId,
    pub(super) dec: bool,
    pub(super) prefix: bool,
}

impl BodyFirChecker<'_> {
    pub(super) fn guard_safe_index(
        &mut self,
        receiver: ExprId,
        origin: OriginId,
        result_ty: ResolvedTy,
        build_selector: impl FnOnce(&mut Self) -> Result<FirExprId, BodyCheckFailure>,
    ) -> Result<FirExprKind, BodyCheckFailure> {
        let initializer = self.expression(receiver)?;
        let receiver_ty = self.expression_type(receiver)?;
        let local = self.allocate_local();
        let declaration = self.body.add_statement(FirStatement {
            origin,
            kind: FirStatementKind::Local {
                target: local,
                ty: receiver_ty,
                mutable: false,
                lateinit: false,
                deferred: false,
                initializer: Some(initializer),
                conversion: None,
            },
        });
        let read = self.body.add_expr(FirExpr {
            origin,
            ty: receiver_ty,
            kind: FirExprKind::ValueRead(local),
        });
        self.expression_substitutions.insert(receiver, read);
        let selector = build_selector(self);
        self.expression_substitutions.remove(&receiver);
        let selector = selector?;
        let guarded = self.body.add_expr(FirExpr {
            origin,
            ty: result_ty,
            kind: FirExprKind::SafeCall {
                receiver: self.safe_call_guard_receiver(FirReceiver {
                    value: read,
                    conversion: None,
                }),
                selector,
            },
        });
        Ok(FirExprKind::Block {
            statements: vec![declaration].into_boxed_slice(),
            result: Some(guarded),
        })
    }

    fn bind_reused_operands(
        &mut self,
        operands: &[ExprId],
        origin: OriginId,
    ) -> Result<Vec<FirStatementId>, BodyCheckFailure> {
        let mut statements = Vec::new();
        for operand in operands {
            if self.expression_substitutions.contains_key(operand) {
                continue;
            }
            let initializer = self.expression(*operand)?;
            let ty = self.expression_type(*operand)?;
            let local = self.allocate_local();
            statements.push(self.body.add_statement(FirStatement {
                origin,
                kind: FirStatementKind::Local {
                    target: local,
                    ty,
                    mutable: false,
                    lateinit: false,
                    deferred: false,
                    initializer: Some(initializer),
                    conversion: None,
                },
            }));
            let read = self.body.add_expr(FirExpr {
                origin,
                ty,
                kind: FirExprKind::ValueRead(local),
            });
            self.expression_substitutions.insert(*operand, read);
        }
        Ok(statements)
    }

    fn release_operands(&mut self, operands: &[ExprId]) {
        for operand in operands {
            self.expression_substitutions.remove(operand);
        }
    }

    pub(super) fn safe_index_read(
        &mut self,
        expression: ExprId,
        receiver: ExprId,
        element: ExprId,
    ) -> Result<FirExprKind, BodyCheckFailure> {
        let origin = self.expression_origin(expression)?;
        let span = self
            .file
            .expr_span(expression)
            .ok_or_else(|| self.failure(None, BodyCheckFailureKind::MissingSourceSpan))?;
        let element_ty = self.expression_type(element)?;
        let result_ty = self.resolved_type(span, Ty::nullable(element_ty.get()))?;
        self.guard_safe_index(receiver, origin, result_ty, |checker| {
            checker.expression(element)
        })
    }

    pub(super) fn safe_index_assignment(
        &mut self,
        statement: StmtId,
        origin: OriginId,
        receiver: ExprId,
        access: ExprId,
        indices: &[ExprId],
        value: ExprId,
    ) -> Result<FirExprId, BodyCheckFailure> {
        let unit = ResolvedTy::new(Ty::Unit).expect("Unit is a publishable FIR type");
        let kind = self.guard_safe_index(receiver, origin, unit, |checker| {
            let compound = checker.is_compound_index_assignment(access, indices, value);
            let mut reused = Vec::new();
            if compound {
                // The member value is the index receiver for both the read and the write.
                reused.push(access);
                reused.extend_from_slice(indices);
            }
            let bindings = checker.bind_reused_operands(&reused, origin)?;
            let write = checker.safe_index_set(statement, origin, access, indices, value);
            checker.release_operands(&reused);
            let write = write?;
            if bindings.is_empty() {
                Ok(write)
            } else {
                Ok(checker.body.add_expr(FirExpr {
                    origin,
                    ty: unit,
                    kind: FirExprKind::Block {
                        statements: bindings.into_boxed_slice(),
                        result: Some(write),
                    },
                }))
            }
        })?;
        Ok(self.body.add_expr(FirExpr {
            origin,
            ty: unit,
            kind,
        }))
    }

    fn safe_index_set(
        &mut self,
        statement: StmtId,
        origin: OriginId,
        access: ExprId,
        indices: &[ExprId],
        value: ExprId,
    ) -> Result<FirExprId, BodyCheckFailure> {
        let convention = ["set", "put"]
            .into_iter()
            .find(|convention| {
                self.info
                    .resolved_stmt_operator_call(statement, convention)
                    .is_some()
            })
            .ok_or_else(|| {
                self.failure(
                    self.file.stmt_spans.get(statement.0 as usize).copied(),
                    BodyCheckFailureKind::UnsupportedStatement(StatementForm::AssignSafeIndex),
                )
            })?;
        let mut operands = indices.to_vec();
        operands.push(value);
        let call =
            self.source_member_statement_operator_call(statement, convention, access, &operands)?;
        let unit = ResolvedTy::new(Ty::Unit).expect("Unit is a publishable FIR type");
        let span = self.file.stmt_spans.get(statement.0 as usize).copied();
        let selected = self
            .info
            .resolved_stmt_operator_call(statement, convention)
            .map(crate::resolve::ResolvedCall::ret)
            .ok_or_else(|| self.failure(span, BodyCheckFailureKind::UnsupportedCallShape))?;
        let call_ty = self.resolved_type(
            span.ok_or_else(|| self.failure(None, BodyCheckFailureKind::MissingSourceSpan))?,
            selected,
        )?;
        let call = self.body.add_expr(FirExpr {
            origin,
            ty: call_ty,
            kind: FirExprKind::Call(call),
        });
        if call_ty == unit {
            return Ok(call);
        }
        let conversion = self
            .selected_type_conversion(call_ty, unit, origin)
            .ok_or_else(|| self.failure(span, BodyCheckFailureKind::UnsupportedCallShape))?;
        Ok(self.body.add_expr(FirExpr {
            origin,
            ty: unit,
            kind: FirExprKind::ImplicitConversion {
                value: call,
                conversion,
            },
        }))
    }

    pub(super) fn safe_index_inc_dec(
        &mut self,
        update: SafeIndexIncDec<'_>,
    ) -> Result<FirExprKind, BodyCheckFailure> {
        let origin = self.expression_origin(update.expression)?;
        let result_ty = self.expression_type(update.expression)?;
        let receiver = update.receiver;
        self.guard_safe_index(receiver, origin, result_ty, |checker| {
            checker.safe_index_update(update, origin)
        })
    }

    fn unit_operator_result(
        &mut self,
        expression: ExprId,
        origin: OriginId,
        kind: FirExprKind,
    ) -> Result<FirExprId, BodyCheckFailure> {
        let unit = ResolvedTy::new(Ty::Unit).expect("Unit is a publishable FIR type");
        let FirExprKind::Call(call) = kind else {
            return Ok(self.body.add_expr(FirExpr {
                origin,
                ty: unit,
                kind,
            }));
        };
        let span = self
            .file
            .expr_span(expression)
            .ok_or_else(|| self.failure(None, BodyCheckFailureKind::MissingSourceSpan))?;
        let selected = self
            .info
            .resolved_operator_call(expression, "set")
            .map(crate::resolve::ResolvedCall::ret)
            .unwrap_or(Ty::Unit);
        let call_ty = self.resolved_type(span, selected)?;
        let call = self.body.add_expr(FirExpr {
            origin,
            ty: call_ty,
            kind: FirExprKind::Call(call),
        });
        if call_ty == unit {
            return Ok(call);
        }
        let conversion = self
            .selected_type_conversion(call_ty, unit, origin)
            .ok_or_else(|| {
                self.failure(
                    Some(span),
                    BodyCheckFailureKind::UnsupportedExpression(ExpressionForm::SafeIndexIncDec),
                )
            })?;
        Ok(self.body.add_expr(FirExpr {
            origin,
            ty: unit,
            kind: FirExprKind::ImplicitConversion {
                value: call,
                conversion,
            },
        }))
    }

    fn safe_index_update(
        &mut self,
        update: SafeIndexIncDec<'_>,
        origin: OriginId,
    ) -> Result<FirExprId, BodyCheckFailure> {
        let SafeIndexIncDec {
            expression,
            access,
            element,
            indices,
            updated,
            dec,
            prefix,
            ..
        } = update;
        let mut reused = Vec::with_capacity(indices.len() + 1);
        reused.push(access);
        reused.extend_from_slice(indices);
        let bindings = self.bind_reused_operands(&reused, origin)?;
        let read = self.expression(element)?;
        let element_ty = self.expression_type(element)?;
        let old = self.allocate_local();
        let mut statements = bindings;
        statements.push(self.body.add_statement(FirStatement {
            origin,
            kind: FirStatementKind::Local {
                target: old,
                ty: element_ty,
                mutable: false,
                lateinit: false,
                deferred: false,
                initializer: Some(read),
                conversion: None,
            },
        }));
        let old_read = self.body.add_expr(FirExpr {
            origin,
            ty: element_ty,
            kind: FirExprKind::ValueRead(old),
        });
        let convention = if dec { "dec" } else { "inc" };
        let updated_ty = self.expression_type(updated)?;
        let updated_kind = if self.selected_operator(expression, convention) {
            FirExprKind::Call(self.source_member_operator_call_on_value(
                expression,
                convention,
                old_read,
                &[],
            )?)
        } else {
            FirExprKind::Unary {
                operation: if dec {
                    FirUnaryOperation::Decrement
                } else {
                    FirUnaryOperation::Increment
                },
                operand: old_read,
            }
        };
        let updated_value = self.body.add_expr(FirExpr {
            origin,
            ty: updated_ty,
            kind: updated_kind,
        });
        self.expression_substitutions.insert(updated, updated_value);
        let set = self.source_member_operator_call(expression, "set", access, &{
            let mut operands = indices.to_vec();
            operands.push(updated);
            operands
        });
        let result = if prefix {
            self.expression(element)
        } else {
            Ok(old_read)
        };
        self.expression_substitutions.remove(&updated);
        self.release_operands(&reused);
        let set = self.unit_operator_result(expression, origin, set?)?;
        let result = result?;
        statements.push(self.body.add_statement(FirStatement {
            origin,
            kind: FirStatementKind::Expression(set),
        }));
        Ok(self.body.add_expr(FirExpr {
            origin,
            ty: element_ty,
            kind: FirExprKind::Block {
                statements: statements.into_boxed_slice(),
                result: Some(result),
            },
        }))
    }
}
