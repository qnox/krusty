//! Safe increment/decrement access expansion.
//!
//! A safe property update evaluates its receiver once and guards the complete read, operator call,
//! and write. Keeping this shape together prevents nullable operator selection and eager updates.

use super::*;

impl Parser<'_> {
    /// Build the store half shared by discarded-value and value-producing member/index inc/dec.
    /// Centralizing this match keeps the accepted lvalue families, source spans, and future property
    /// or index-store extensions identical across both syntactic contexts.
    pub(super) fn finish_incdec_access_assignment(
        &mut self,
        target: ExprId,
        value: ExprId,
        start: Span,
        target_span: Span,
    ) -> Option<StmtId> {
        let statement = match self.file.expr(target).clone() {
            Expr::Member { receiver, name } => Stmt::AssignMember {
                receiver,
                name,
                value,
                safe: false,
            },
            Expr::SafeCall {
                receiver,
                name,
                args: None,
            } => Stmt::AssignMember {
                receiver,
                name,
                value,
                safe: true,
            },
            Expr::Index { array, indices } => Stmt::AssignIndex {
                array,
                indices,
                value,
            },
            _ => return None,
        };
        Some(self.finish_assignment_stmt(statement, start, target_span))
    }

    pub(super) fn incdec_target(
        &mut self,
        e: ExprId,
        dec: bool,
        prefix: bool,
        op_span: Span,
        start: Span,
    ) -> StmtId {
        let target_span = self.assignment_target_span(e);
        match self.file.expr(e).clone() {
            Expr::Name(n) => self.parse_incdec(n, dec, prefix, start, target_span),
            _ => {
                self.diags.error(
                    op_span,
                    "krusty: '++'/'--' is only supported on a variable, property, or indexed access",
                );
                self.finish_stmt(Stmt::Expr(e), start)
            }
        }
    }

    pub(super) fn parse_incdec(
        &mut self,
        name: String,
        dec: bool,
        prefix: bool,
        start: Span,
        target: Span,
    ) -> StmtId {
        self.finish_assignment_stmt(Stmt::IncDec { name, dec, prefix }, start, target)
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn safe_incdec_access_value_expr(
        &mut self,
        e: ExprId,
        target: ExprId,
        dec: bool,
        prefix: bool,
        start: Span,
        target_span: Span,
    ) -> Option<ExprId> {
        let Expr::SafeCall {
            receiver,
            name,
            args: None,
        } = self.file.expr(target).clone()
        else {
            return None;
        };
        const RECEIVER: &str = "$$incDecReceiver";
        let receiver_local = self.file.add_stmt(
            Stmt::Local {
                is_var: false,
                name: RECEIVER.to_string(),
                ty: None,
                init: receiver,
            },
            start,
        );
        let receiver_read = self
            .file
            .add_expr(Expr::Name(RECEIVER.to_string()), target_span);
        let member = self.file.add_expr(
            Expr::Member {
                receiver: receiver_read,
                name,
            },
            target_span,
        );
        let update = self.incdec_access_value_expr(e, member, dec, prefix, start);

        let condition_receiver = self
            .file
            .add_expr(Expr::Name(RECEIVER.to_string()), target_span);
        let null = self.file.add_expr(Expr::NullLit, target_span);
        let condition = self.file.add_expr(
            Expr::Binary {
                op: BinOp::Ne,
                lhs: condition_receiver,
                rhs: null,
                operator_span: target_span,
            },
            target_span,
        );
        let null_result = self.file.add_expr(Expr::NullLit, target_span);
        let guarded = self.file.add_expr(
            Expr::If {
                cond: condition,
                then_branch: update,
                else_branch: Some(null_result),
            },
            target_span,
        );
        Some(self.file.add_expr(
            Expr::Block {
                stmts: vec![receiver_local],
                trailing: Some(guarded),
            },
            Span::new(start.lo, self.file.expr_spans[e.0 as usize].hi),
        ))
    }

    /// Drop the saved old/new result when a non-safe access increment is used as a statement. Safe
    /// updates have no operand record on their guarded outer block, so the conditional stays live.
    pub(super) fn discard_incdec_access_value(&mut self, expression: ExprId) -> bool {
        if !self.file.incdec_access_operands.contains_key(&expression) {
            return false;
        }
        let original = match self.file.expr(expression) {
            Expr::Block { stmts, .. } => stmts.iter().position(|&statement| {
                matches!(
                    self.file.stmt(statement),
                    Stmt::Local { name, .. } if name == "$$incDecOriginal"
                )
            }),
            _ => return false,
        };
        let Expr::Block { stmts, trailing } = &mut self.file.expr_arena[expression.0 as usize]
        else {
            return false;
        };
        if let Some(original) = original {
            stmts.remove(original);
        }
        *trailing = None;
        true
    }
}
