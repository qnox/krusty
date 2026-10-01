//! Checked FIR for an unparenthesized safe index.
//!
//! The source receiver is one compiler-generated local. `get`, `set`, and `inc`/`dec` run only
//! after that local is non-null, on the member selected for that branch. Index operands reused by
//! an update are bound inside the branch, so a null receiver does not evaluate them.
//!
//! Substitutions are a stack. Each binding restores the entry it replaced, including when lowering
//! fails, so an enclosing assignment or `when` subject stays bound for the rest of its scope.

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

struct SubstitutionRestore {
    key: ExprId,
    previous: Option<FirExprId>,
}

struct BoundOperand {
    key: ExprId,
    previous: Option<FirExprId>,
    statement: FirStatementId,
}

impl BodyFirChecker<'_> {
    fn restore_substitution(&mut self, saved: SubstitutionRestore) {
        match saved.previous {
            Some(previous) => {
                self.expression_substitutions.insert(saved.key, previous);
            }
            None => {
                self.expression_substitutions.remove(&saved.key);
            }
        }
    }

    fn restore_bound(&mut self, bound: &[BoundOperand]) {
        for entry in bound.iter().rev() {
            self.restore_substitution(SubstitutionRestore {
                key: entry.key,
                previous: entry.previous,
            });
        }
    }

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
        let previous = self.expression_substitutions.insert(receiver, read);
        let selector = build_selector(self);
        self.restore_substitution(SubstitutionRestore {
            key: receiver,
            previous,
        });
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
    ) -> Result<Vec<BoundOperand>, BodyCheckFailure> {
        let mut bound = Vec::new();
        for operand in operands {
            if self.expression_substitutions.contains_key(operand) {
                continue;
            }
            let entry = match self.bind_fresh_operand(*operand, origin) {
                Ok(entry) => entry,
                Err(err) => {
                    self.restore_bound(&bound);
                    return Err(err);
                }
            };
            bound.push(entry);
        }
        Ok(bound)
    }

    fn bind_fresh_operand(
        &mut self,
        operand: ExprId,
        origin: OriginId,
    ) -> Result<BoundOperand, BodyCheckFailure> {
        let initializer = self.expression(operand)?;
        let ty = self.expression_type(operand)?;
        let local = self.allocate_local();
        let statement = self.body.add_statement(FirStatement {
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
        });
        let read = self.body.add_expr(FirExpr {
            origin,
            ty,
            kind: FirExprKind::ValueRead(local),
        });
        let previous = self.expression_substitutions.insert(operand, read);
        Ok(BoundOperand {
            key: operand,
            previous,
            statement,
        })
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

    pub(super) fn safe_index_inc_dec_expr(
        &mut self,
        expression: ExprId,
    ) -> Result<FirExprKind, BodyCheckFailure> {
        let Expr::SafeIndexIncDec {
            receiver,
            access,
            element,
            indices,
            updated,
            dec,
            prefix,
        } = self.file.expr(expression).clone()
        else {
            unreachable!("a safe index update is a safe-index increment");
        };
        self.safe_index_inc_dec(SafeIndexIncDec {
            expression,
            receiver,
            access,
            element,
            indices: &indices,
            updated,
            dec,
            prefix,
        })
    }

    pub(super) fn safe_index_assignment_stmt(
        &mut self,
        statement: StmtId,
        origin: OriginId,
    ) -> Result<FirExprId, BodyCheckFailure> {
        let Stmt::AssignSafeIndex {
            receiver,
            access,
            indices,
            value,
            ..
        } = self.file.stmt(statement).clone()
        else {
            unreachable!("a safe index store is an assignment to a safe index");
        };
        self.safe_index_assignment(statement, origin, receiver, access, &indices, value)
    }

    /// The parser represents `receiver[indices] op= rhs` as a write whose value contains the
    /// matching indexed read and reuses every operand identity. Checked FIR binds those operands
    /// once before publishing the read-modify-write, just as it does for compound member access.
    pub(super) fn is_compound_index_assignment(
        &self,
        receiver: ExprId,
        indices: &[ExprId],
        value: ExprId,
    ) -> bool {
        let Expr::Binary { lhs, .. } = self.file.expr(value) else {
            return false;
        };
        matches!(
            self.file.expr(*lhs),
            Expr::Index {
                array: read_receiver,
                indices: read_indices,
            } if *read_receiver == receiver && read_indices == indices
        )
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
            let bound = checker.bind_reused_operands(&reused, origin)?;
            let write = checker.safe_index_set(statement, origin, access, indices, value);
            checker.restore_bound(&bound);
            let write = write?;
            if bound.is_empty() {
                Ok(write)
            } else {
                Ok(checker.body.add_expr(FirExpr {
                    origin,
                    ty: unit,
                    kind: FirExprKind::Block {
                        statements: bound
                            .iter()
                            .map(|entry| entry.statement)
                            .collect::<Vec<_>>()
                            .into_boxed_slice(),
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
        let span = self.file.expr_span(expression);
        let FirExprKind::Call(call) = kind else {
            return Err(self.failure(span, BodyCheckFailureKind::UnsupportedCallShape));
        };
        let span =
            span.ok_or_else(|| self.failure(None, BodyCheckFailureKind::MissingSourceSpan))?;
        let selected = self
            .info
            .resolved_operator_call(expression, "set")
            .map(crate::resolve::ResolvedCall::ret)
            .ok_or_else(|| self.failure(Some(span), BodyCheckFailureKind::UnsupportedCallShape))?;
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
            .ok_or_else(|| self.failure(Some(span), BodyCheckFailureKind::UnsupportedCallShape))?;
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
        let bound = self.bind_reused_operands(&reused, origin)?;
        let read = match self.expression(element) {
            Ok(read) => read,
            Err(err) => {
                self.restore_bound(&bound);
                return Err(err);
            }
        };
        let element_ty = match self.expression_type(element) {
            Ok(ty) => ty,
            Err(err) => {
                self.restore_bound(&bound);
                return Err(err);
            }
        };
        let old = self.allocate_local();
        let mut statements = bound
            .iter()
            .map(|entry| entry.statement)
            .collect::<Vec<_>>();
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
        let updated_ty = match self.expression_type(updated) {
            Ok(ty) => ty,
            Err(err) => {
                self.restore_bound(&bound);
                return Err(err);
            }
        };
        let updated_kind = if self.selected_operator(expression, convention) {
            match self.source_member_operator_call_on_value(expression, convention, old_read, &[]) {
                Ok(call) => FirExprKind::Call(call),
                Err(err) => {
                    self.restore_bound(&bound);
                    return Err(err);
                }
            }
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
        let previous_updated = self.expression_substitutions.insert(updated, updated_value);
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
        self.restore_substitution(SubstitutionRestore {
            key: updated,
            previous: previous_updated,
        });
        self.restore_bound(&bound);
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

#[cfg(test)]
mod tests {
    use super::super::test_support::checked_function_body_rewriting;
    use super::*;
    use crate::features::LangFeatures;
    use crate::libraries::EmptySymbolSource;

    fn rewrite_index_to_when_subject(
        file: &mut crate::ast::File,
        types: &mut crate::resolve::TypeInfo,
    ) {
        let subject = file.expr_arena.iter().find_map(|expr| match expr {
            Expr::When {
                subject: Some(subject),
                ..
            } => Some(*subject),
            _ => None,
        });
        let subject = subject.expect("when subject");
        let update = file
            .expr_arena
            .iter()
            .position(|expr| matches!(expr, Expr::SafeIndexIncDec { .. }))
            .map(|index| ExprId(index as u32))
            .expect("safe index update");
        let Expr::SafeIndexIncDec {
            element, indices, ..
        } = file.expr_arena[update.0 as usize].clone()
        else {
            unreachable!("safe index update");
        };
        let previous = indices;
        if let Expr::SafeIndexIncDec { indices, .. } = &mut file.expr_arena[update.0 as usize] {
            *indices = vec![subject];
        }
        if let Expr::Index { indices, .. } = &mut file.expr_arena[element.0 as usize] {
            *indices = vec![subject];
        }
        // Operator selection stays on the literal's `Int` type. Argument slots still name that
        // literal, so point them at the subject node the update now shares.
        for commitment in types.resolved_call_arg_slots.values_mut() {
            for slot in &mut commitment.slots {
                if slot.is_some_and(|argument| previous.contains(&argument)) {
                    *slot = Some(subject);
                }
            }
        }
        for slots in types.resolved_stmt_operator_arg_slots.values_mut() {
            for slot in slots {
                if slot.is_some_and(|argument| previous.contains(&argument)) {
                    *slot = Some(subject);
                }
            }
        }
    }

    fn subject_calls(body: &FirBody, index: &crate::fir::signature::ResolvedModuleIndex) -> usize {
        (0..body.expression_count())
            .filter(|&raw| {
                let Some(expr) = body.expr(FirExprId::from_raw(raw as u32)) else {
                    return false;
                };
                let FirExprKind::Call(call) = &expr.kind else {
                    return false;
                };
                call.target
                    .module()
                    .is_some_and(|callable| index.callable_name(callable) == Some("subject"))
            })
            .count()
    }

    #[test]
    fn safe_index_update_keeps_an_enclosing_subject_substitution() {
        let source = "\
class Cell {\n\
    operator fun get(i: Int): Cell = this\n\
    operator fun set(i: Int, value: Cell) {}\n\
    operator fun inc(): Cell = this\n\
}\n\
operator fun Cell?.contains(i: Int): Boolean = true\n\
class Holder { val cell: Cell = Cell() }\n\
fun subject(): Int = 1\n\
fun use(holder: Holder?): Boolean {\n\
    return when (subject()) {\n\
        in holder?.cell[0]++ -> true\n\
        else -> false\n\
    }\n\
}\n";
        let (body, index) = checked_function_body_rewriting(
            source,
            "use",
            Box::new(EmptySymbolSource),
            &LangFeatures::new(),
            rewrite_index_to_when_subject,
        );
        assert_eq!(
            subject_calls(&body, &index),
            1,
            "the when subject is already bound; the safe index must not drop that substitution"
        );
    }
}
