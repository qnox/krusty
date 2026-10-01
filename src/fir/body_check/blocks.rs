//! Checked FIR for blocks, including a block whose value is a fun-interface lambda.

use std::collections::HashMap;

use super::*;

impl BodyFirChecker<'_> {
    pub(super) fn block(
        &mut self,
        expression: ExprId,
        statements: &[StmtId],
        trailing: Option<ExprId>,
    ) -> Result<FirExprKind, BodyCheckFailure> {
        self.scopes.push(HashMap::new());
        self.delegate_scopes.push(HashMap::new());
        self.local_callable_scopes.push(HashMap::new());
        let result = self.checked_block_with_shared_operands(expression, statements, trailing);
        self.scopes.pop();
        self.delegate_scopes.pop();
        self.local_callable_scopes.pop();
        result
    }

    /// Publish the evaluate-once contract selected for a parser-expanded access increment. The
    /// resolver has already removed package/classifier/`super` qualifiers from this list, leaving
    /// only runtime operands in source order. Bind those values before checking the generated
    /// read-modify-write block and replace every shared AST occurrence with its FIR value identity.
    fn checked_block_with_shared_operands(
        &mut self,
        expression: ExprId,
        statements: &[StmtId],
        trailing: Option<ExprId>,
    ) -> Result<FirExprKind, BodyCheckFailure> {
        let operands = match self.info.expr_lowers.get(&expression) {
            Some(ExprLowering::IncDecAccessOperands(operands)) => operands.clone(),
            _ => return self.block_in_current_scope(expression, statements, trailing),
        };
        let origin = self.expression_origin(expression)?;
        let mut bindings = Vec::with_capacity(operands.len());
        let mut inserted = Vec::with_capacity(operands.len());
        for operand in operands {
            if self.expression_substitutions.contains_key(&operand) {
                continue;
            }
            let initializer = self.expression(operand)?;
            let ty = self.expression_type(operand)?;
            let target = self.allocate_local();
            bindings.push(self.body.add_statement(FirStatement {
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
            }));
            let replacement = self.body.add_expr(FirExpr {
                origin,
                ty,
                kind: FirExprKind::ValueRead(target),
            });
            self.expression_substitutions.insert(operand, replacement);
            inserted.push(operand);
        }
        let checked = self.block_in_current_scope(expression, statements, trailing);
        for operand in inserted {
            self.expression_substitutions.remove(&operand);
        }
        let FirExprKind::Block { statements, result } = checked? else {
            unreachable!("checking a block must produce a FIR block")
        };
        bindings.extend(statements);
        Ok(FirExprKind::Block {
            statements: bindings.into_boxed_slice(),
            result,
        })
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
