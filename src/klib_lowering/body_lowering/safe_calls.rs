//! The producer's safe-call temporary, its exact null proof and the guarded selector, lowered as
//! checked FIR lowers `receiver?.selector`.

use super::{mismatch, BodyLowering, Lowered};
use crate::ir::{ExprId, IrBinOp, IrConst, IrExpr, IrTypeOp};
use crate::metadata::klib_ir::tree::{KlibIrExprId, KlibIrExprKind, KlibIrStatement};
use crate::metadata::klib_ir::{KlibIrConstant, KlibIrSymbolKind};
use crate::types::Ty;

impl BodyLowering<'_> {
    /// `{ val tmp = receiver; when { tmp == null -> null; else -> selector } }`, whose selector
    /// reads `tmp` through its implicit non-null cast.
    pub(super) fn safe_call_block(
        &mut self,
        statements: &[KlibIrStatement],
        ty: Ty,
    ) -> Lowered<ExprId> {
        let &[KlibIrStatement::Variable(temporary), KlibIrStatement::Expression(conditional)] =
            statements
        else {
            return Err(mismatch(
                "a safe call does not contain one temporary and a conditional",
            ));
        };
        let variable = self.arena.variable(temporary);
        if variable.base.origin != "IR_TEMPORARY_VARIABLE"
            || variable.base.symbol.kind != KlibIrSymbolKind::Variable
        {
            return Err(mismatch(
                "a safe call does not declare a compiler temporary",
            ));
        }
        let symbol = variable.base.symbol.clone();
        let input = super::semantic_type(self.arena, self.header.type_parameters(), variable.ty)?;
        if !input.is_nullable() || !ty.is_nullable() {
            return Err(mismatch("a safe call is not over and of nullable types"));
        }
        let expression = self.arena.expr(conditional);
        let KlibIrExprKind::When {
            branches,
            origin: None,
        } = &expression.kind
        else {
            return Err(mismatch(
                "a safe call does not end in its origin-less conditional",
            ));
        };
        let [absent, present] = branches.as_slice() else {
            return Err(mismatch(
                "a safe-call conditional does not have two branches",
            ));
        };
        if self.ty(expression, "safe-call conditional")? != ty
            || !matches!(
                self.arena.expr(present.condition).kind,
                KlibIrExprKind::Const(KlibIrConstant::Boolean(true))
            )
        {
            return Err(mismatch(
                "a safe-call conditional has another type or else condition",
            ));
        }
        let (read, null) = self.elvis_null_test(absent.condition)?;
        let read = self.arena.expr(read);
        let null = self.arena.expr(null);
        let absent_result = self.arena.expr(absent.result);
        if !matches!(&read.kind, KlibIrExprKind::GetValue { symbol: read, .. } if *read == symbol)
            || self.ty(read, "safe-call temporary read")? != input
            || !matches!(null.kind, KlibIrExprKind::Const(KlibIrConstant::Null))
            || !matches!(
                absent_result.kind,
                KlibIrExprKind::Const(KlibIrConstant::Null)
            )
            || self.ty(absent_result, "safe-call null result")? != Ty::nullable(Ty::Nothing)
        {
            return Err(mismatch(
                "a safe-call null proof does not name its exact temporary",
            ));
        }
        let variable = self.unnamed_local(temporary)?;
        let local = self
            .locals
            .get(&symbol)
            .expect("the safe-call temporary was declared");
        let condition_read = self.ir.add_expr(IrExpr::GetValue(local.index));
        let null = self.ir.add_expr(IrExpr::Const(IrConst::Null));
        let condition = self.ir.add_expr(IrExpr::PrimitiveBinOp {
            op: IrBinOp::Eq,
            lhs: condition_read,
            rhs: null,
        });
        let null_result = self.ir.add_expr(IrExpr::Const(IrConst::Null));
        self.null_proven.insert(symbol.clone());
        let selector = self.expression(present.result);
        self.null_proven.remove(&symbol);
        let selector = self.coercion(selector?, ty);
        let guarded = self.ir.add_expr(IrExpr::When {
            branches: vec![(Some(condition), null_result), (None, selector)],
        });
        self.ir.whens.exhaustive.insert(guarded, ty);
        self.ir.logical_types.insert(guarded, ty);
        self.ir.null_guards.insert(guarded);
        let block = self.ir.add_expr(IrExpr::Block {
            stmts: vec![variable],
            value: Some(guarded),
        });
        self.ir.safe_calls.insert(block);
        Ok(self.typed(block, ty))
    }

    /// The selector's read of its safe-call temporary, cast to the non-null type its null test
    /// proves: an anonymous snapshot read under the conversion checked FIR lowering places there.
    /// `None` for any other cast.
    pub(super) fn null_proven_receiver(
        &mut self,
        argument: KlibIrExprId,
        target: Ty,
    ) -> Option<ExprId> {
        let KlibIrExprKind::GetValue { symbol, .. } = &self.arena.expr(argument).kind else {
            return None;
        };
        if !self.null_proven.contains(symbol) || target.is_nullable() {
            return None;
        }
        let local = self.locals.get(symbol)?;
        if Ty::nullable(target) != local.ty {
            return None;
        }
        let read = self.ir.add_expr(IrExpr::GetValue(local.index));
        Some(self.ir.add_expr(IrExpr::TypeOp {
            op: IrTypeOp::ImplicitCoercion,
            arg: read,
            type_operand: target.canonical_semantic(),
        }))
    }
}
