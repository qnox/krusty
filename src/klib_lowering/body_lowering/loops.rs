//! `while`, `do`-`while` and `for` loops and the `break`s and `continue`s that leave them.
//!
//! Checked FIR lowering gives every loop a label of its own and names that label from each jump,
//! so a jump names its loop rather than the innermost one. A KLIB names a jump's loop by the
//! loop's file-unique identity; the label lowering gives a loop is built from that identity and
//! the unit function, so it is unique in the unit and names nothing else.

use super::{mismatch, unsupported, BodyLowering, Lowered};
use crate::ir::{ExprId, IrExpr};
use crate::metadata::klib_ir::tree::{KlibIrExprKind, KlibIrLoop, KlibIrStatement};
use crate::types::Ty;

impl BodyLowering<'_> {
    /// A source `while` (`post_test` false) or `do`-`while` loop. Checked FIR lowering lowers its
    /// condition, then its body block, and records no type for the loop: it is a statement. The
    /// condition is lowered first for a `do`-`while` too, so a condition that reads a variable
    /// its body declares declines as a read of a value the body does not declare yet.
    pub(super) fn loop_statement(
        &mut self,
        serialized: &KlibIrLoop,
        post_test: bool,
        ty: Ty,
    ) -> Lowered<ExprId> {
        if ty != Ty::Unit {
            return Err(mismatch("a loop is not typed `Unit`"));
        }
        let expected = if post_test {
            "DO_WHILE_LOOP"
        } else {
            "WHILE_LOOP"
        };
        if serialized.origin.as_deref() != Some(expected) {
            return unsupported("a loop the compiler introduced");
        }
        let Some(body) = serialized.body else {
            return unsupported("a loop without a body");
        };
        let KlibIrExprKind::Block {
            statements,
            origin: None,
        } = &self.arena.expr(body).kind
        else {
            return unsupported("a loop whose body is not a block");
        };
        self.labeled_loop(serialized, post_test, statements)
    }

    /// A `for` loop, which a KLIB serializes as kotlinc desugars it and checked FIR lowering lowers
    /// an iterator-protocol loop: an unnamed local holding the iterable's iterator, then a `while`
    /// loop on its `hasNext()` whose body block declares the loop variable from its `next()` before
    /// the source body.
    pub(super) fn for_loop(&mut self, statements: &[KlibIrStatement], ty: Ty) -> Lowered<ExprId> {
        if ty != Ty::Unit {
            return Err(mismatch("a `for` loop is not typed `Unit`"));
        }
        let [KlibIrStatement::Variable(iterator), KlibIrStatement::Expression(inner)] = statements
        else {
            return unsupported("a `for` loop of another shape than an iterator and its loop");
        };
        if self.arena.variable(*iterator).base.origin != "FOR_LOOP_ITERATOR" {
            return unsupported("a `for` loop of another shape than an iterator and its loop");
        }
        let KlibIrExprKind::While(serialized) = &self.arena.expr(*inner).kind else {
            return unsupported("a `for` loop of another shape than an iterator and its loop");
        };
        if serialized.origin.as_deref() != Some("FOR_LOOP_INNER_WHILE") {
            return unsupported("a `for` loop of another shape than an iterator and its loop");
        }
        let Some(body) = serialized.body else {
            return unsupported("a loop without a body");
        };
        let KlibIrExprKind::Block {
            statements: body,
            origin,
        } = &self.arena.expr(body).kind
        else {
            return unsupported("a loop whose body is not a block");
        };
        let declares_variable = matches!(
            body.first(),
            Some(KlibIrStatement::Variable(variable))
                if self.arena.variable(*variable).base.origin == "FOR_LOOP_VARIABLE"
        );
        if !declares_variable || !matches!(origin.as_deref(), None | Some("FOR_LOOP_INNER_WHILE")) {
            return unsupported("a `for` loop body that does not open with its variable");
        }
        let iterator = self.unnamed_local(*iterator)?;
        let lowered = self.labeled_loop(serialized, false, body)?;
        Ok(self.ir.add_expr(IrExpr::Block {
            stmts: vec![iterator, lowered],
            value: None,
        }))
    }

    /// A loop labeled for the jumps that name `serialized`, whose body block holds `statements`.
    fn labeled_loop(
        &mut self,
        serialized: &KlibIrLoop,
        post_test: bool,
        statements: &[KlibIrStatement],
    ) -> Lowered<ExprId> {
        if self.loops.iter().any(|(id, _)| *id == serialized.id) {
            return Err(mismatch("a loop repeats an enclosing loop's identity"));
        }
        let label = format!("$klib_loop_{}_{}", self.function.function, serialized.id);
        self.loops.push((serialized.id, label.clone()));
        let lowered = self
            .operand(serialized.condition, Ty::Boolean)
            .and_then(|cond| {
                let body = self.block(statements)?;
                Ok((cond, body))
            });
        self.loops.pop();
        let (cond, body) = lowered?;
        Ok(self.ir.add_expr(IrExpr::While {
            cond,
            body,
            update: None,
            post_test,
            label: Some(label),
        }))
    }

    /// `break` (`resume` false) or `continue` (`resume` true) of the enclosing loop `loop_id`.
    pub(super) fn jump(&mut self, loop_id: u32, ty: Ty, resume: bool) -> Lowered<ExprId> {
        if ty != Ty::Nothing {
            return Err(mismatch("a `break` or `continue` is not typed `Nothing`"));
        }
        let Some(label) = self
            .loops
            .iter()
            .rev()
            .find(|(id, _)| *id == loop_id)
            .map(|(_, label)| label.clone())
        else {
            return Err(mismatch("a `break` or `continue` names no enclosing loop"));
        };
        if !resume {
            self.breakable_loops.insert(loop_id);
        }
        let label = Some(label);
        Ok(self.ir.add_expr(if resume {
            IrExpr::Continue { label }
        } else {
            IrExpr::Break { label }
        }))
    }
}
