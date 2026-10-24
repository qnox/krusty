//! `try` expressions.
//!
//! A `try` lowers as checked FIR lowers the source expression: its body, each `catch` clause with
//! its parameter bound to the next value slot under its source name, and its `finally` block. A
//! `try` used as a value converts each branch to its type as a conditional's branches are.

use super::{mismatch, semantic_type, BodyLowering, Lowered};
use crate::ir::{ExprId, IrCatch, IrCatchBinding, IrExpr};
use crate::metadata::klib_ir::tree::{KlibIrCatch, KlibIrExprId};
use crate::types::Ty;

impl BodyLowering<'_> {
    pub(super) fn try_expression(
        &mut self,
        body: KlibIrExprId,
        catches: &[KlibIrCatch],
        finally: Option<KlibIrExprId>,
        ty: Ty,
    ) -> Lowered<ExprId> {
        let body = self.try_branch(body, ty)?;
        let catches = catches
            .iter()
            .map(|catch| self.catch_clause(catch, ty))
            .collect::<Lowered<Vec<_>>>()?;
        let finally = finally
            .map(|finally| self.expression(finally))
            .transpose()?;
        Ok(self.ir.add_expr(IrExpr::Try {
            body,
            catches,
            finally,
            result: ty,
        }))
    }

    /// One `catch` clause: its parameter declared in the next value slot, then its body.
    fn catch_clause(&mut self, catch: &KlibIrCatch, ty: Ty) -> Lowered<IrCatch> {
        let parameter = self.arena.variable(catch.parameter);
        let caught = semantic_type(self.arena, self.header.type_parameters(), parameter.ty)?;
        if caught.non_null().obj_internal().is_none() {
            return Err(mismatch("a `catch` parameter is not of a class type"));
        }
        if parameter.initializer.is_some() {
            return Err(mismatch("a `catch` parameter has an initializer"));
        }
        let local = self.locals.catch_parameter(parameter, caught)?;
        self.locals.declare(&parameter.base.symbol, local)?;
        self.locals
            .record_name(&parameter.base.symbol, &parameter.name);
        Ok(IrCatch {
            var: local.index,
            binding: Some(IrCatchBinding::source(parameter.name.clone())),
            ty: caught,
            body: self.try_branch(catch.result, ty)?,
            line: None,
        })
    }

    /// The body or a `catch` clause's result: a statement of a `Unit` `try`, else a value of its
    /// type.
    fn try_branch(&mut self, branch: KlibIrExprId, ty: Ty) -> Lowered<ExprId> {
        let lowered = self.expression(branch)?;
        if ty == Ty::Unit {
            return Ok(lowered);
        }
        self.branch_value(lowered, ty)
    }
}
