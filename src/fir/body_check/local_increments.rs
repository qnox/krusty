//! Checked FIR for `++` and `--` on a local, captured local, or local-class capture.
//!
//! A postfix update snapshots its storage once, writes the incremented snapshot, and yields that
//! snapshot. A prefix update writes the incremented current value and yields a fresh storage read.
//! Statement position keeps the same checked shape; target-specific contraction happens later.

use super::*;
use crate::fir::{FirStatementKind, LocalBinding};

#[derive(Clone, Copy)]
enum IncrementStorage {
    Local(LocalValueId),
    Captured {
        enclosing_depth: u32,
        binding: LocalBinding,
    },
    Class(ClassCaptureBinding),
}

#[derive(Clone, Copy)]
enum IncrementSite {
    Expression { expression: ExprId, target: ExprId },
    Statement(StmtId),
}

impl BodyFirChecker<'_> {
    fn increment_storage(&self, name: &str) -> Option<IncrementStorage> {
        if let Some(local) = self.local(name) {
            return Some(IncrementStorage::Local(local));
        }
        if let Some((enclosing_depth, binding)) = self.outer_values.get(name).copied() {
            return Some(IncrementStorage::Captured {
                enclosing_depth,
                binding,
            });
        }
        self.class_values
            .get(name)
            .copied()
            .map(IncrementStorage::Class)
    }

    fn increment_storage_type(&self, name: &str, storage: IncrementStorage) -> ResolvedTy {
        match storage {
            IncrementStorage::Local(_) => {
                self.local_binding(name)
                    .expect("the increment target is a bound local")
                    .ty
            }
            IncrementStorage::Captured { binding, .. } => binding.ty,
            IncrementStorage::Class(binding) => binding.ty,
        }
    }

    fn increment_read_kind(
        &mut self,
        storage: IncrementStorage,
        origin: OriginId,
    ) -> Result<FirExprKind, BodyCheckFailure> {
        Ok(match storage {
            IncrementStorage::Local(local) => FirExprKind::ValueRead(local),
            IncrementStorage::Captured {
                enclosing_depth,
                binding,
            } => FirExprKind::CapturedValueRead {
                enclosing_depth,
                source: binding.value,
            },
            IncrementStorage::Class(binding) => self.class_storage_read_kind(binding, origin)?,
        })
    }

    fn increment_write_kind(
        &mut self,
        storage: IncrementStorage,
        origin: OriginId,
        value: FirExprId,
        conversion: Option<FirConversion>,
    ) -> Result<FirExprKind, BodyCheckFailure> {
        Ok(match storage {
            IncrementStorage::Local(target) => FirExprKind::ValueWrite {
                target,
                value,
                conversion,
            },
            IncrementStorage::Captured {
                enclosing_depth,
                binding,
            } => FirExprKind::CapturedValueWrite {
                enclosing_depth,
                source: binding.value,
                value,
                conversion,
            },
            IncrementStorage::Class(binding) => {
                self.class_storage_shared_write_kind(binding, origin, value, conversion)?
            }
        })
    }

    fn record_increment_capture(&mut self, storage: IncrementStorage, origin: OriginId) {
        if let IncrementStorage::Captured {
            enclosing_depth,
            binding,
        } = storage
        {
            self.body.add_capture(FirCapture {
                origin,
                enclosing_depth,
                source: FirCaptureSource::Value(binding.value),
                ty: binding.ty,
                shared_cell: true,
            });
        }
    }

    fn incremented(
        &mut self,
        site: IncrementSite,
        decrement: bool,
        operand: FirExprId,
    ) -> Result<FirExprKind, BodyCheckFailure> {
        let convention = if decrement { "dec" } else { "inc" };
        let selected = match site {
            IncrementSite::Expression { expression, .. } => {
                self.selected_operator(expression, convention)
            }
            IncrementSite::Statement(statement) => self
                .info
                .resolved_stmt_operator_call(statement, convention)
                .is_some(),
        };
        if !selected {
            return Ok(FirExprKind::Unary {
                operation: if decrement {
                    FirUnaryOperation::Decrement
                } else {
                    FirUnaryOperation::Increment
                },
                operand,
            });
        }
        match site {
            IncrementSite::Expression { expression, target } => {
                let previous = self.expression_substitutions.insert(target, operand);
                let call = self.zero_arg_expression_operator_call(expression, convention, target);
                match previous {
                    Some(previous) => {
                        self.expression_substitutions.insert(target, previous);
                    }
                    None => {
                        self.expression_substitutions.remove(&target);
                    }
                }
                call
            }
            IncrementSite::Statement(statement) => {
                self.zero_arg_statement_operator_call_on_value(statement, convention, operand)
            }
        }
    }

    pub(super) fn increment_local_expression(
        &mut self,
        expression: ExprId,
        target: ExprId,
        decrement: bool,
        prefix: bool,
    ) -> Result<FirExprId, BodyCheckFailure> {
        let Expr::Name(name) = self.file.expr(target) else {
            return Err(self.failure(
                self.file.expr_span(expression),
                BodyCheckFailureKind::UnsupportedExpression(ExpressionForm::IncDec),
            ));
        };
        if let Some((depth, delegate)) = self.delegated_binding(name) {
            let kind = self.delegated_inc_dec_expression(
                expression, target, decrement, prefix, depth, delegate,
            )?;
            return self.add_expression(expression, kind);
        }
        let Some(storage) = self.increment_storage(name) else {
            if let Some(kind) =
                self.property_inc_dec_expression(expression, target, decrement, prefix)?
            {
                return self.add_expression(expression, kind);
            }
            return Err(self.failure(
                self.file.expr_span(expression),
                BodyCheckFailureKind::UnknownLocal,
            ));
        };
        let origin = self.expression_origin(expression)?;
        self.record_increment_capture(storage, origin);
        let resolution = self
            .info
            .resolved_inc_dec
            .get(&IncDecSite::Expression(expression))
            .copied()
            .ok_or_else(|| {
                self.failure(
                    self.file.expr_span(expression),
                    BodyCheckFailureKind::UnsupportedExpression(ExpressionForm::IncDec),
                )
            })?;
        let span = self
            .file
            .expr_span(expression)
            .ok_or_else(|| self.failure(None, BodyCheckFailureKind::MissingSourceSpan))?;
        let receiver_ty = self.resolved_type(span, resolution.receiver_ty)?;
        let storage_ty = self.increment_storage_type(name, storage);
        let site = IncrementSite::Expression { expression, target };
        let mut statements = Vec::new();
        let (updated_kind, old_value) = if prefix {
            let operand = self.expression(target)?;
            (self.incremented(site, decrement, operand)?, None)
        } else {
            let (declaration, temporary) =
                self.increment_temporary(storage, (receiver_ty, receiver_ty), None, origin)?;
            statements.push(declaration);
            let operand = self.body.add_expr(FirExpr {
                origin,
                ty: receiver_ty,
                kind: FirExprKind::ValueRead(temporary),
            });
            (self.incremented(site, decrement, operand)?, Some(temporary))
        };
        let updated = self.add_expression(expression, updated_kind)?;
        statements.push(self.increment_write(storage, storage_ty, updated, origin)?);
        let result_kind = match old_value {
            Some(old_value) => FirExprKind::ValueRead(old_value),
            None => self.increment_read_kind(storage, origin)?,
        };
        let result = self.body.add_expr(FirExpr {
            origin,
            ty: self.expression_type(expression)?,
            kind: result_kind,
        });
        let block = self.add_expression(
            expression,
            FirExprKind::Block {
                statements: statements.into_boxed_slice(),
                result: Some(result),
            },
        )?;
        if !prefix {
            self.body.record_postfix_update(block);
        }
        Ok(block)
    }

    pub(super) fn increment_local_statement(
        &mut self,
        statement: StmtId,
        name: &str,
        decrement: bool,
        prefix: bool,
        origin: OriginId,
    ) -> Result<Option<FirStatementKind>, BodyCheckFailure> {
        let Some(storage) = self.increment_storage(name) else {
            return Ok(None);
        };
        let resolution = self
            .info
            .resolved_inc_dec
            .get(&IncDecSite::Statement(statement))
            .copied()
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
        self.record_increment_capture(storage, origin);
        let storage_ty = self.increment_storage_type(name, storage);
        let receiver_ty = self.resolved_type(span, resolution.receiver_ty)?;
        let updated_ty = self.resolved_type(span, resolution.updated_ty)?;
        let read_conversion = self
            .selected_type_conversion(storage_ty, receiver_ty, origin)
            .or_else(|| {
                (storage_ty != receiver_ty).then_some(FirConversion {
                    origin,
                    kind: FirConversionKind::SmartCast { to: receiver_ty },
                })
            });
        let site = IncrementSite::Statement(statement);
        let mut statements = Vec::new();
        let (operand, old_value) = if prefix {
            let read_kind = self.increment_read_kind(storage, origin)?;
            let storage_read = self.body.add_expr(FirExpr {
                origin,
                ty: storage_ty,
                kind: read_kind,
            });
            (
                self.convert_fir_value(storage_read, receiver_ty, read_conversion),
                None,
            )
        } else {
            let (declaration, temporary) = self.increment_temporary(
                storage,
                (storage_ty, receiver_ty),
                read_conversion,
                origin,
            )?;
            statements.push(declaration);
            let operand = self.body.add_expr(FirExpr {
                origin,
                ty: receiver_ty,
                kind: FirExprKind::ValueRead(temporary),
            });
            (operand, Some(temporary))
        };
        let updated_kind = self.incremented(site, decrement, operand)?;
        let updated = self.body.add_expr(FirExpr {
            origin,
            ty: updated_ty,
            kind: updated_kind,
        });
        statements.push(self.increment_write(storage, storage_ty, updated, origin)?);
        let (result_ty, result_kind) = match old_value {
            Some(old_value) => (receiver_ty, FirExprKind::ValueRead(old_value)),
            None => (storage_ty, self.increment_read_kind(storage, origin)?),
        };
        let result = self.body.add_expr(FirExpr {
            origin,
            ty: result_ty,
            kind: result_kind,
        });
        let block = self.body.add_expr(FirExpr {
            origin,
            ty: result_ty,
            kind: FirExprKind::Block {
                statements: statements.into_boxed_slice(),
                result: Some(result),
            },
        });
        if !prefix {
            self.body.record_postfix_update(block);
        }
        Ok(Some(FirStatementKind::Expression(block)))
    }

    fn increment_temporary(
        &mut self,
        storage: IncrementStorage,
        (read_ty, ty): (ResolvedTy, ResolvedTy),
        conversion: Option<FirConversion>,
        origin: OriginId,
    ) -> Result<(FirStatementId, LocalValueId), BodyCheckFailure> {
        let temporary = self.allocate_local();
        let read_kind = self.increment_read_kind(storage, origin)?;
        let read = self.body.add_expr(FirExpr {
            origin,
            ty: read_ty,
            kind: read_kind,
        });
        let declaration = self.body.add_statement(FirStatement {
            origin,
            kind: FirStatementKind::Local {
                target: temporary,
                ty,
                mutable: false,
                lateinit: false,
                deferred: false,
                initializer: Some(read),
                conversion,
            },
        });
        Ok((declaration, temporary))
    }

    fn increment_write(
        &mut self,
        storage: IncrementStorage,
        storage_ty: ResolvedTy,
        updated: FirExprId,
        origin: OriginId,
    ) -> Result<FirStatementId, BodyCheckFailure> {
        let updated_ty = self
            .body
            .expr(updated)
            .expect("the checked increment result exists")
            .ty;
        let conversion = self.selected_type_conversion(updated_ty, storage_ty, origin);
        let kind = self.increment_write_kind(storage, origin, updated, conversion)?;
        let write = self.body.add_expr(FirExpr {
            origin,
            ty: ResolvedTy::new(Ty::Unit).expect("Unit is a publishable FIR type"),
            kind,
        });
        Ok(self.body.add_statement(FirStatement {
            origin,
            kind: FirStatementKind::Expression(write),
        }))
    }
}
