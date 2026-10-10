//! Blocks as checked FIR lowers source blocks.
//!
//! A source block keeps its last statement apart as the block's value when that statement is an
//! expression; a declaration, an assignment, a loop or a jump stays a statement. The block's
//! checked type is then its value's type, `Nothing` when it ends in a jump, and `Unit` otherwise.
//! A KLIB does not keep that distinction: it types a block by where it is used, and coerces each
//! statement of another type to `Unit`. This module rebuilds the source shape from the statement
//! forms, so a block lowers to the block checked FIR lowering produces for its source.

use super::{mismatch, unsupported, BodyLowering, Lowered};
use crate::ir::{ExprId, IrExpr};
use crate::metadata::klib_ir::tree::{
    KlibIrBranch, KlibIrExprId, KlibIrExprKind, KlibIrStatement, KlibIrTypeOperator,
};
use crate::metadata::klib_ir::KlibIrConstant;
use crate::types::Ty;

use super::super::decline::KlibBodyDeclineReason;

impl BodyLowering<'_, '_, '_> {
    /// The function's body. Checked FIR lowering lowers a block body as the source block, typed
    /// by its checked type, inside the callable's own block; both are the callable's scope. A
    /// function whose result is not `Unit` must leave its body through a jump, so that body is
    /// typed `Nothing`; one that can reach its end declines.
    pub(in super::super) fn body(mut self, statements: &[KlibIrStatement]) -> Lowered<ExprId> {
        let source = self.block(statements)?;
        if self.header.result() != Ty::Unit && self.lowered_type(source) != Ty::Nothing {
            return Err(KlibBodyDeclineReason::MissingReturn);
        }
        let body = self.ir.add_expr(IrExpr::Block {
            stmts: vec![source],
            value: None,
        });
        self.ir.callable_scopes.insert(source);
        self.ir.callable_scopes.insert(body);
        Ok(body)
    }

    /// A source block of `statements`, typed as checked FIR lowering types it.
    pub(super) fn block(&mut self, statements: &[KlibIrStatement]) -> Lowered<ExprId> {
        if let Some((_, leading)) = statements.split_last() {
            // The checker erases what follows a jump, so a block that keeps it has no source.
            if leading
                .iter()
                .any(|statement| self.statement_diverges(statement))
            {
                return unsupported("a statement after a jump");
            }
        }
        let trailing = match statements.last() {
            Some(KlibIrStatement::Expression(last)) if self.is_source_expression(*last) => {
                Some(*last)
            }
            _ => None,
        };
        let leading = match trailing {
            Some(_) => &statements[..statements.len() - 1],
            None => statements,
        };
        let stmts = leading
            .iter()
            .map(|statement| self.statement(statement))
            .collect::<Lowered<Vec<_>>>()?;
        let value = trailing
            .map(|trailing| self.discarded(trailing))
            .transpose()?;
        let ty = match value {
            Some(value) => self.lowered_type(value),
            None if statements
                .last()
                .is_some_and(|statement| self.is_jump_statement(statement)) =>
            {
                Ty::Nothing
            }
            None => Ty::Unit,
        };
        let block = self.ir.add_expr(IrExpr::Block { stmts, value });
        Ok(self.typed(block, ty))
    }

    /// `id`, a statement form, in the place of an expression, as checked FIR lowering places a
    /// source statement there: inside a block of its own.
    pub(super) fn statement_block(&mut self, id: KlibIrExprId) -> Lowered<ExprId> {
        let jump = matches!(
            self.arena.expr(id).kind,
            KlibIrExprKind::Return { .. }
                | KlibIrExprKind::Break { .. }
                | KlibIrExprKind::Continue { .. }
        );
        let statement = self.expression(id)?;
        let block = self.ir.add_expr(IrExpr::Block {
            stmts: vec![statement],
            value: None,
        });
        Ok(self.typed(block, if jump { Ty::Nothing } else { Ty::Unit }))
    }

    /// Whether `id` is an expression of the source, rather than a statement (an assignment, a
    /// loop or a jump), and so a block's value when it ends one.
    pub(super) fn is_source_expression(&self, id: KlibIrExprId) -> bool {
        !matches!(
            self.arena.expr(id).kind,
            KlibIrExprKind::Return { .. }
                | KlibIrExprKind::Break { .. }
                | KlibIrExprKind::Continue { .. }
                | KlibIrExprKind::SetValue { .. }
                | KlibIrExprKind::While(_)
                | KlibIrExprKind::DoWhile(_)
        )
    }

    fn is_jump_statement(&self, statement: &KlibIrStatement) -> bool {
        matches!(
            statement,
            KlibIrStatement::Expression(id) if matches!(
                self.arena.expr(*id).kind,
                KlibIrExprKind::Return { .. }
                    | KlibIrExprKind::Break { .. }
                    | KlibIrExprKind::Continue { .. }
            )
        )
    }

    /// Whether the checker reads `statement` as always transferring control, which makes every
    /// later statement of its block unreachable.
    fn statement_diverges(&self, statement: &KlibIrStatement) -> bool {
        match statement {
            KlibIrStatement::Expression(id) => self.diverges(*id),
            _ => false,
        }
    }

    /// The checker's syntactic divergence of a source expression: a jump or `throw`, a block
    /// ending in one, an `if` with an `else` all of whose branches diverge, and a call with a
    /// diverging argument. A subjectless `when` is not read as diverging.
    fn diverges(&self, id: KlibIrExprId) -> bool {
        match &self.arena.expr(id).kind {
            KlibIrExprKind::Throw(_)
            | KlibIrExprKind::Return { .. }
            | KlibIrExprKind::Break { .. }
            | KlibIrExprKind::Continue { .. } => true,
            KlibIrExprKind::TypeOperator {
                operator: KlibIrTypeOperator::ImplicitCoercionToUnit,
                argument,
                ..
            } => self.diverges(*argument),
            KlibIrExprKind::Block {
                statements,
                origin: None,
            } => match statements.last() {
                Some(KlibIrStatement::Expression(last)) => self.diverges(*last),
                _ => false,
            },
            KlibIrExprKind::When {
                branches,
                origin: Some(origin),
            } if origin == "IF" => {
                self.has_else(branches)
                    && branches.iter().all(|branch| self.diverges(branch.result))
            }
            KlibIrExprKind::Call { access, .. } => super::calls::arguments(access)
                .into_iter()
                .flatten()
                .any(|argument| self.diverges(argument)),
            _ => false,
        }
    }

    /// Whether the last of `branches` is a KLIB `else`: a branch whose condition is the constant
    /// `true`.
    pub(super) fn has_else(&self, branches: &[KlibIrBranch]) -> bool {
        branches.last().is_some_and(|branch| {
            matches!(
                self.arena.expr(branch.condition).kind,
                KlibIrExprKind::Const(KlibIrConstant::Boolean(true))
            )
        })
    }

    /// Whether checked FIR gives the `Unit` value `id` a `Unit` value of its own where a `Unit`
    /// result is required: a call, a subjectless `when`, an assignment and a block without a
    /// value do, an `if` and a read do not (the checker's `unit_effect_requires_value`).
    pub(super) fn unit_effect_requires_value(&self, id: KlibIrExprId) -> Lowered<bool> {
        Ok(match &self.arena.expr(id).kind {
            KlibIrExprKind::Call { .. } | KlibIrExprKind::SetValue { .. } => true,
            KlibIrExprKind::When { origin, .. } => origin.as_deref() == Some("WHEN"),
            KlibIrExprKind::Block {
                statements,
                origin: None,
            } => match statements.last() {
                Some(KlibIrStatement::Expression(last)) if self.is_source_expression(*last) => {
                    self.unit_effect_requires_value(self.coerced_value(*last))?
                }
                _ => true,
            },
            KlibIrExprKind::Return { .. }
            | KlibIrExprKind::Break { .. }
            | KlibIrExprKind::Continue { .. }
            | KlibIrExprKind::While(_)
            | KlibIrExprKind::DoWhile(_) => true,
            KlibIrExprKind::TypeOperator {
                operator: KlibIrTypeOperator::ImplicitCoercionToUnit,
                ..
            } => return Err(mismatch("a `Unit` value is coerced to `Unit`")),
            _ => false,
        })
    }

    /// The value a KLIB coerces to `Unit` at `id`, or `id` itself.
    pub(super) fn coerced_value(&self, id: KlibIrExprId) -> KlibIrExprId {
        match &self.arena.expr(id).kind {
            KlibIrExprKind::TypeOperator {
                operator: KlibIrTypeOperator::ImplicitCoercionToUnit,
                argument,
                ..
            } => *argument,
            _ => id,
        }
    }
}
