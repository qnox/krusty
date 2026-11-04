//! Checked FIR for blocks, including a block whose value is a fun-interface lambda.

use crate::fx_hash::HashMap;

use super::*;
use crate::ast::IncDecAccess;

impl BodyFirChecker<'_> {
    pub(super) fn block(
        &mut self,
        expression: ExprId,
        statements: &[StmtId],
        trailing: Option<ExprId>,
    ) -> Result<FirExprKind, BodyCheckFailure> {
        self.scopes.push(HashMap::default());
        self.delegate_scopes.push(HashMap::default());
        self.local_callable_scopes.push(HashMap::default());
        let result = self.checked_block_with_shared_operands(expression, statements, trailing);
        self.scopes.pop();
        self.delegate_scopes.pop();
        self.local_callable_scopes.pop();
        result
    }

    /// A parser-expanded member or index `++`/`--`, built as kotlinc's FIR builds it.
    ///
    /// The resolver has already removed package/classifier/`super` qualifiers from the operand
    /// list, leaving only runtime operands in source order. Each one that is not trivial is bound
    /// once before the access and every shared AST occurrence reads that binding. A foldable
    /// operand ([`Self::is_foldable_operand`]) is read again at each use instead.
    ///
    /// A postfix update keeps the old value in a temporary, writes `temporary.inc()` and yields
    /// the temporary; a prefix update writes `read.inc()` and yields a second read of the access.
    /// A statement keeps the same shape: the temporary is still stored, and a prefix statement
    /// still reads the access again.
    fn checked_block_with_shared_operands(
        &mut self,
        expression: ExprId,
        statements: &[StmtId],
        trailing: Option<ExprId>,
    ) -> Result<FirExprKind, BodyCheckFailure> {
        let access = match (
            self.info.expr_lowers.get(&expression),
            self.file.incdec_accesses.get(&expression),
        ) {
            (Some(ExprLowering::IncDecAccessOperands(operands)), Some(access)) => {
                (operands.clone(), access.clone())
            }
            _ => return self.block_in_current_scope(expression, statements, trailing),
        };
        let (operands, access) = access;
        let origin = self.expression_origin(expression)?;
        let mut bindings = Vec::with_capacity(operands.len());
        let mut inserted = Vec::with_capacity(operands.len() + 1);
        for operand in operands {
            if self.expression_substitutions.contains_key(&operand) {
                continue;
            }
            let initializer = self.expression(operand)?;
            if self.is_foldable_operand(initializer) {
                continue;
            }
            let ty = self.expression_type(operand)?;
            let (binding, replacement, _) = self.bind_temporary(origin, ty, initializer);
            bindings.push(binding);
            self.expression_substitutions.insert(operand, replacement);
            inserted.push(operand);
        }
        let checked =
            self.access_increment(expression, &access, statements, trailing, &mut inserted);
        for operand in inserted {
            self.expression_substitutions.remove(&operand);
        }
        let (statements, result) = checked?;
        bindings.extend(statements);
        Ok(FirExprKind::Block {
            statements: bindings.into_boxed_slice(),
            result,
        })
    }

    pub(super) fn bind_temporary(
        &mut self,
        origin: OriginId,
        ty: ResolvedTy,
        initializer: FirExprId,
    ) -> (FirStatementId, FirExprId, LocalValueId) {
        let target = self.allocate_local();
        let binding = self.body.add_statement(FirStatement {
            origin,
            kind: FirStatementKind::Local {
                target,
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
            kind: FirExprKind::ValueRead(target),
        });
        (binding, read, target)
    }

    /// The read, update and write of one expanded access, then whatever the parser kept after
    /// them (a prefix statement's discarded re-read) and the value.
    fn access_increment(
        &mut self,
        expression: ExprId,
        access: &IncDecAccess,
        statements: &[StmtId],
        trailing: Option<ExprId>,
        substituted: &mut Vec<ExprId>,
    ) -> Result<(Vec<FirStatementId>, Option<FirExprId>), BodyCheckFailure> {
        let Stmt::Local { init: read, .. } = *self.file.stmt(access.value) else {
            unreachable!("an expanded access increment binds its read first")
        };
        let Stmt::IncDec { dec, .. } = *self.file.stmt(access.update) else {
            unreachable!("an expanded access increment updates its read")
        };
        let stored = match self.file.stmt(access.store) {
            Stmt::AssignMember { value, .. } | Stmt::AssignIndex { value, .. } => *value,
            _ => unreachable!("an expanded access increment writes a member or an index"),
        };
        let origin = self.statement_origin(access.update)?;
        let span = self
            .file
            .stmt_spans
            .get(access.update.0 as usize)
            .copied()
            .ok_or_else(|| self.failure(None, BodyCheckFailureKind::MissingSourceSpan))?;
        let resolution = self
            .info
            .resolved_inc_dec
            .get(&IncDecSite::Statement(access.update))
            .copied()
            .ok_or_else(|| {
                self.failure(
                    Some(span),
                    BodyCheckFailureKind::UnsupportedStatement(StatementForm::IncDec),
                )
            })?;
        let read_ty = self.expression_type(read)?;
        let first_read = self.expression(read)?;
        let mut checked = Vec::new();
        let (operand, old_value) = if access.prefix {
            (first_read, None)
        } else {
            let (binding, value, old_value) = self.bind_temporary(origin, read_ty, first_read);
            checked.push(binding);
            (value, Some((old_value, read_ty)))
        };
        let receiver_ty = self.resolved_type(span, resolution.receiver_ty)?;
        let conversion = self.selected_type_conversion(read_ty, receiver_ty, origin);
        let operand = self.convert_fir_value(operand, receiver_ty, conversion);
        let convention = if dec { "dec" } else { "inc" };
        let updated_kind = if self
            .info
            .resolved_stmt_operator_call(access.update, convention)
            .is_some()
        {
            self.zero_arg_statement_operator_call_on_value(access.update, convention, operand)?
        } else {
            FirExprKind::Unary {
                operation: if dec {
                    FirUnaryOperation::Decrement
                } else {
                    FirUnaryOperation::Increment
                },
                operand,
            }
        };
        let updated = self.body.add_expr(FirExpr {
            origin,
            ty: self.resolved_type(span, resolution.updated_ty)?,
            kind: updated_kind,
        });
        self.expression_substitutions.insert(stored, updated);
        substituted.push(stored);
        for &statement in statements {
            let replaced = [Some(access.value), Some(access.update), access.original];
            if !replaced.contains(&Some(statement)) {
                checked.push(self.statement(statement)?);
            }
        }
        let result = match (trailing, old_value) {
            (None, _) => None,
            (Some(_), Some((old_value, ty))) => Some(self.body.add_expr(FirExpr {
                origin,
                ty,
                kind: FirExprKind::ValueRead(old_value),
            })),
            (Some(trailing), None) => {
                Some(self.trailing_value_with_recorded_sam(expression, trailing)?)
            }
        };
        Ok((checked, result))
    }

    /// Build a block after its owner has opened the lexical scope. Do-while owns that scope because
    /// its body declarations remain visible while the trailing condition is checked.
    pub(super) fn block_in_current_scope(
        &mut self,
        block: ExprId,
        statements: &[StmtId],
        trailing: Option<ExprId>,
    ) -> Result<FirExprKind, BodyCheckFailure> {
        for statement in statements {
            if matches!(self.file.stmt(*statement), Stmt::LocalFun(_)) {
                let callable = self.body.allocate_local_callable();
                self.local_callable_scopes
                    .last_mut()
                    .expect("a FIR block owns a local callable scope")
                    .insert(*statement, callable);
            }
        }
        let checked = statements
            .iter()
            // A `contract { … }` block is not executable code: it is a compile-time declaration whose
            // lambda uses the `ContractBuilder` DSL, and kotlinc emits no bytecode for it. The checker
            // marks it `StmtLowering::Erased` and deliberately does not type its body, so FIR must
            // drop it rather than try to build a call from an unchecked DSL shape.
            .filter(|statement| {
                !matches!(
                    self.info.stmt_lowers.get(statement),
                    Some(StmtLowering::Erased)
                )
            })
            .map(|statement| self.statement(*statement))
            .collect::<Result<Vec<_>, _>>();
        let result = checked.as_ref().map_or(Ok(None), |_| {
            trailing
                .map(|trailing| self.trailing_value_with_recorded_sam(block, trailing))
                .transpose()
        });
        Ok(FirExprKind::Block {
            statements: checked?.into_boxed_slice(),
            result: result?,
        })
    }
}
