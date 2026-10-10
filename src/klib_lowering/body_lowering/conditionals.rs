//! `if` chains, subjectless `when`s and the short-circuit operators.
//!
//! A KLIB serializes all of them as one `when` whose origin names the source form, with an `else`
//! as a final branch whose condition is the constant `true`. Each lowers as checked FIR lowers
//! that form: `&&` and `||` as a two-branch `when` recorded as a short circuit, an `if` chain as
//! one nested two-branch `when` per `if`, and a subjectless `when` as one flat `when`.

use super::{mismatch, unsupported, BodyLowering, Lowered};
use crate::ir::{complete_bottom_value, ExprId, IrConst, IrExpr, IrShortCircuitKind};
use crate::metadata::klib_ir::tree::{KlibIrBranch, KlibIrExprId, KlibIrExprKind};
use crate::metadata::klib_ir::KlibIrConstant;
use crate::types::Ty;

use super::super::decline::KlibBodyDeclineReason;

impl BodyLowering<'_, '_, '_> {
    /// A `when` of the source form `origin`, typed `ty`. The branches are lowered before the
    /// shape is checked, so a body that uses an unmodelled form inside any other `when` declines
    /// by that form.
    pub(super) fn conditional(
        &mut self,
        branches: &[KlibIrBranch],
        origin: Option<&str>,
        ty: Ty,
    ) -> Lowered<ExprId> {
        match origin {
            Some("IF") => self.if_chain(branches, ty),
            Some("WHEN") => self.subjectless_when(branches, ty),
            Some("ANDAND") => self.short_circuit(branches, ty, IrShortCircuitKind::And),
            Some("OROR") => self.short_circuit(branches, ty, IrShortCircuitKind::Or),
            _ => unsupported("a `when` of another origin than `if`, `when`, `&&` or `||`"),
        }
    }

    /// `a && b`, serialized as `when { a -> b; else -> false }`, or `a || b`, serialized as
    /// `when { a -> true; else -> b }`. Checked FIR lowering builds the same `when` around the
    /// two lowered operands, with a constant of its own, and records it as the short circuit.
    fn short_circuit(
        &mut self,
        branches: &[KlibIrBranch],
        ty: Ty,
        kind: IrShortCircuitKind,
    ) -> Lowered<ExprId> {
        let [first, second] = branches else {
            return Err(mismatch(
                "a short-circuit operator has other than two branches",
            ));
        };
        if ty != Ty::Boolean || !self.has_else(branches) {
            return Err(mismatch(
                "a short-circuit operator is not a `Boolean` `when` with an `else`",
            ));
        }
        let (rhs, constant) = match kind {
            IrShortCircuitKind::And => (first.result, second.result),
            IrShortCircuitKind::Or => (second.result, first.result),
        };
        let expected = kind == IrShortCircuitKind::Or;
        if !matches!(
            self.arena.expr(constant).kind,
            KlibIrExprKind::Const(KlibIrConstant::Boolean(value)) if value == expected
        ) {
            return Err(mismatch(
                "a short-circuit operator's constant branch is not its operator's",
            ));
        }
        let lhs = self.operand(first.condition, Ty::Boolean)?;
        let rhs = self.operand(rhs, Ty::Boolean)?;
        let constant = self.ir.add_expr(IrExpr::Const(IrConst::Boolean(expected)));
        let branches = match kind {
            IrShortCircuitKind::And => vec![(Some(lhs), rhs), (None, constant)],
            IrShortCircuitKind::Or => vec![(Some(lhs), constant), (None, rhs)],
        };
        let when = self.ir.add_expr(IrExpr::When { branches });
        self.ir.short_circuits.insert(when, kind);
        Ok(when)
    }

    /// The branches of `branches` that have a condition, and the `else` result if the last
    /// branch is an `else`.
    fn split_else<'b>(
        &self,
        branches: &'b [KlibIrBranch],
    ) -> Lowered<(&'b [KlibIrBranch], Option<KlibIrExprId>)> {
        let (conditions, otherwise) = match (self.has_else(branches), branches.split_last()) {
            (true, Some((otherwise, conditions))) => {
                let marker = self.arena.expr(otherwise.condition);
                if self.ty(marker, "`else` condition")? != Ty::Boolean {
                    return Err(KlibBodyDeclineReason::ImplicitConversion);
                }
                (conditions, Some(otherwise.result))
            }
            _ => (branches, None),
        };
        if conditions.is_empty() {
            return unsupported("a `when` without a condition");
        }
        Ok((conditions, otherwise))
    }

    /// A subjectless `when`. With an `else` it is one flat `when` recorded as exhaustive at its
    /// type; without one it is a `Unit` statement checked FIR lowering records nothing for. Its
    /// branch results are lowered as written: the checker converts none of them.
    fn subjectless_when(&mut self, branches: &[KlibIrBranch], ty: Ty) -> Lowered<ExprId> {
        let (conditions, otherwise) = self.split_else(branches)?;
        if otherwise.is_none() && ty != Ty::Unit {
            return Err(mismatch("a `when` without an `else` is not typed `Unit`"));
        }
        let mut lowered = Vec::with_capacity(branches.len());
        for branch in conditions {
            let condition = self.operand(branch.condition, Ty::Boolean)?;
            lowered.push((Some(condition), self.when_result(branch.result, ty)?));
        }
        if let Some(otherwise) = otherwise {
            lowered.push((None, self.when_result(otherwise, ty)?));
        }
        let when = self.ir.add_expr(IrExpr::When { branches: lowered });
        if otherwise.is_some() {
            self.ir.whens.exhaustive.insert(when, ty);
        }
        Ok(when)
    }

    /// A subjectless `when`'s branch result: the value itself, of the `when`'s type or `Nothing`,
    /// or, in a `Unit` `when`, whatever the branch evaluates.
    fn when_result(&mut self, result: KlibIrExprId, ty: Ty) -> Lowered<ExprId> {
        if ty == Ty::Unit {
            return self.branch(result, true);
        }
        let lowered = self.branch(result, false)?;
        self.expect_value(lowered, ty)?;
        Ok(lowered)
    }

    /// An `if`-`else if` chain. `if (a) x else if (b) y else z` is `if (a) x else (if (b) y else
    /// z)`: checked FIR lowering nests one two-branch `when` per `if`, gives a missing final
    /// `else` an empty `Unit` block, and records a `Unit` `if` as an exhaustive statement. Each
    /// inner `if` is an expression of the chain's type, as the expression lowering of the
    /// outermost one records it.
    fn if_chain(&mut self, branches: &[KlibIrBranch], ty: Ty) -> Lowered<ExprId> {
        let (conditions, otherwise) = self.split_else(branches)?;
        let mut lowered = Vec::with_capacity(conditions.len());
        for branch in conditions {
            let condition = self.operand(branch.condition, Ty::Boolean)?;
            lowered.push((condition, self.if_result(branch.result, ty)?));
        }
        let mut otherwise = match otherwise {
            Some(otherwise) => self.if_result(otherwise, ty)?,
            None if ty == Ty::Unit => {
                let empty = self.ir.add_expr(IrExpr::Block {
                    stmts: Vec::new(),
                    value: None,
                });
                self.typed(empty, Ty::Unit)
            }
            None => return Err(mismatch("an `if` without an `else` is not typed `Unit`")),
        };
        for (index, (condition, result)) in lowered.into_iter().enumerate().rev() {
            let conditional = self.ir.add_expr(IrExpr::When {
                branches: vec![(Some(condition), result), (None, otherwise)],
            });
            // The checker records a `Unit` `if` as an exhaustive statement, whether or not it has
            // an `else`.
            if ty == Ty::Unit {
                self.ir.whens.exhaustive.insert(conditional, Ty::Unit);
            }
            otherwise = if index == 0 {
                conditional
            } else {
                self.ir.logical_types.insert(conditional, ty);
                let completed = complete_bottom_value(self.ir, conditional, ty);
                self.ir.logical_types.insert(completed, ty);
                completed
            };
        }
        Ok(otherwise)
    }

    /// An `if` branch converted to the `if`'s type `ty` as the checker converts it. A value of
    /// another type than `ty` declines, unless it is `Nothing`, which needs no conversion. In a
    /// `Unit` `if`, a branch of any other type, and a `Unit` effect the checker gives a value of
    /// its own, is followed by the `Unit` value.
    fn if_result(&mut self, result: KlibIrExprId, ty: Ty) -> Lowered<ExprId> {
        if ty != Ty::Unit {
            let lowered = self.branch(result, false)?;
            self.expect_value(lowered, ty)?;
            return Ok(lowered);
        }
        let lowered = self.branch(result, true)?;
        let converted = match self.lowered_type(lowered) {
            Ty::Nothing => false,
            Ty::Unit => self.unit_effect_requires_value(self.coerced_value(result))?,
            _ => true,
        };
        if !converted {
            return Ok(lowered);
        }
        let unit = self.ir.add_expr(IrExpr::UnitInstance);
        let block = self.ir.add_expr(IrExpr::Block {
            stmts: vec![lowered],
            value: Some(unit),
        });
        self.ir.logical_types.insert(block, Ty::Unit);
        Ok(block)
    }

    /// A branch result. A statement form is placed in a block of its own, as the source places
    /// it; a branch whose value is `discarded` drops the KLIB's coercion to `Unit`.
    fn branch(&mut self, result: KlibIrExprId, discarded: bool) -> Lowered<ExprId> {
        if !self.is_source_expression(result) {
            return self.statement_block(result);
        }
        if discarded {
            self.discarded(result)
        } else {
            self.expression(result)
        }
    }

    /// `lowered` is a value of type `ty`, or diverges.
    fn expect_value(&self, lowered: ExprId, ty: Ty) -> Lowered<()> {
        match self.lowered_type(lowered) {
            actual if actual == ty || actual == Ty::Nothing => Ok(()),
            _ => Err(KlibBodyDeclineReason::ImplicitConversion),
        }
    }
}
