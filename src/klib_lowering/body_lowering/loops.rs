//! `while` and `do`-`while` loops and the `break`s and `continue`s that leave them.
//!
//! Checked FIR lowering gives every loop a label of its own and names that label from each jump,
//! so a jump names its loop rather than the innermost one. A KLIB names a jump's loop by the
//! loop's file-unique identity; the label lowering gives a loop is built from that identity and
//! the unit function, so it is unique in the unit and names nothing else.

use super::{mismatch, unsupported, BodyLowering, Lowered};
use crate::ir::{ExprId, IrExpr};
use crate::metadata::klib_ir::tree::{KlibIrExprKind, KlibIrLoop};
use crate::types::Ty;

impl BodyLowering<'_, '_, '_> {
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
        let Some((_, label)) = self.loops.iter().rev().find(|(id, _)| *id == loop_id) else {
            return Err(mismatch("a `break` or `continue` names no enclosing loop"));
        };
        let label = Some(label.clone());
        Ok(self.ir.add_expr(if resume {
            IrExpr::Continue { label }
        } else {
            IrExpr::Break { label }
        }))
    }
}
