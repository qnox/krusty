//! Casts that have already run, for a later operand of the same expression.
//!
//! A non-null `as` proves its operand's type for every read that cannot run until the cast has
//! been evaluated. Short-circuit right operands (`&&` / `||`) are not among those reads: the
//! left operand can finish without them. Positive `&&` facts still see both sides, because a
//! true conjunction evaluated both.

use crate::ast::{BinOp, Expr, ExprId, File};
use crate::diag::Span;

use super::{
    ExpressionForm, OriginId, SigExpr, SigExprId, SignatureConstraintExtractor, SignatureScopeId,
};

impl SignatureConstraintExtractor {
    /// `is` / null facts, plus every non-null `as` that ran while `condition` evaluated to true.
    pub(super) fn positive_smartcasts(
        &mut self,
        file: &File,
        condition: ExprId,
        scope: SignatureScopeId,
        origin: &mut impl FnMut(Span) -> OriginId,
    ) -> Result<Vec<(Box<str>, SigExprId)>, ExpressionForm> {
        let mut bindings = self.positive_smartcast_facts(file, condition, scope, origin)?;
        // A true `&&` recurses into both operands, and each operand records its own casts.
        // Walking the conjunction here would see only its left operand.
        if !matches!(file.expr(condition), Expr::Binary { op: BinOp::And, .. }) {
            bindings.extend(self.evaluated_as_cast_bindings(file, condition, scope, origin)?);
        }
        Ok(bindings)
    }

    /// Non-null `as` casts whose operands certainly ran because `expression` was evaluated.
    pub(super) fn evaluated_as_cast_bindings(
        &mut self,
        file: &File,
        expression: ExprId,
        scope: SignatureScopeId,
        origin: &mut impl FnMut(Span) -> OriginId,
    ) -> Result<Vec<(Box<str>, SigExprId)>, ExpressionForm> {
        let mut bindings = Vec::new();
        self.collect_evaluated_as_casts(file, expression, scope, origin, &mut bindings)?;
        Ok(bindings)
    }

    fn collect_evaluated_as_casts(
        &mut self,
        file: &File,
        expression: ExprId,
        scope: SignatureScopeId,
        origin: &mut impl FnMut(Span) -> OriginId,
        bindings: &mut Vec<(Box<str>, SigExprId)>,
    ) -> Result<(), ExpressionForm> {
        match file.expr(expression) {
            Expr::As {
                operand,
                ty,
                nullable,
            } => {
                self.collect_evaluated_as_casts(file, *operand, scope, origin, bindings)?;
                if *nullable {
                    return Ok(());
                }
                let Expr::Name(name) = file.expr(*operand) else {
                    return Ok(());
                };
                let target = self.smartcast_type(file, *operand, ty, scope, origin)?;
                let previous = bindings
                    .iter()
                    .rev()
                    .find_map(|(bound, value)| (bound.as_ref() == name.as_str()).then_some(*value))
                    .or_else(|| {
                        self.lexical_values
                            .iter()
                            .rev()
                            .find_map(|values| values.get(name.as_str()).copied())
                    });
                let narrowed = match previous {
                    Some(value) => self.graph.add_expr(SigExpr::CastNarrowed {
                        value,
                        target,
                        scope,
                    }),
                    None => target,
                };
                bindings.push((name.clone().into_boxed_str(), narrowed));
            }
            Expr::Binary { op, lhs, rhs, .. } => {
                self.collect_evaluated_as_casts(file, *lhs, scope, origin, bindings)?;
                if !matches!(op, BinOp::And | BinOp::Or) {
                    self.collect_evaluated_as_casts(file, *rhs, scope, origin, bindings)?;
                }
            }
            Expr::Elvis { lhs, .. } => {
                self.collect_evaluated_as_casts(file, *lhs, scope, origin, bindings)?;
            }
            Expr::Unary { operand, .. } | Expr::NotNull { operand } | Expr::Is { operand, .. } => {
                self.collect_evaluated_as_casts(file, *operand, scope, origin, bindings)?;
            }
            Expr::Member { receiver, .. } | Expr::SafeCall { receiver, .. } => {
                self.collect_evaluated_as_casts(file, *receiver, scope, origin, bindings)?;
            }
            Expr::Index { array, indices } => {
                self.collect_evaluated_as_casts(file, *array, scope, origin, bindings)?;
                for index in indices {
                    self.collect_evaluated_as_casts(file, *index, scope, origin, bindings)?;
                }
            }
            Expr::Call { callee, args } => {
                self.collect_evaluated_as_casts(file, *callee, scope, origin, bindings)?;
                for argument in args {
                    self.collect_evaluated_as_casts(file, *argument, scope, origin, bindings)?;
                }
            }
            Expr::InRange {
                value, start, end, ..
            } => {
                self.collect_evaluated_as_casts(file, *value, scope, origin, bindings)?;
                self.collect_evaluated_as_casts(file, *start, scope, origin, bindings)?;
                self.collect_evaluated_as_casts(file, *end, scope, origin, bindings)?;
            }
            Expr::RangeTo { lo, hi, .. } => {
                self.collect_evaluated_as_casts(file, *lo, scope, origin, bindings)?;
                self.collect_evaluated_as_casts(file, *hi, scope, origin, bindings)?;
            }
            Expr::If { cond, .. } => {
                self.collect_evaluated_as_casts(file, *cond, scope, origin, bindings)?;
            }
            _ => {}
        }
        Ok(())
    }
}
