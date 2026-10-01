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
        let mut evaluated = Vec::new();
        crate::ast::definitely_evaluated::for_each_in_expression(
            file,
            expression,
            &mut |expression| evaluated.push(expression),
        );
        self.evaluated_as_cast_bindings_from(file, &evaluated, scope, origin)
    }

    pub(super) fn evaluated_as_cast_bindings_from(
        &mut self,
        file: &File,
        evaluated: &[ExprId],
        scope: SignatureScopeId,
        origin: &mut impl FnMut(Span) -> OriginId,
    ) -> Result<Vec<(Box<str>, SigExprId)>, ExpressionForm> {
        let mut bindings: Vec<(Box<str>, SigExprId)> = Vec::new();
        for &expression in evaluated {
            let Expr::As {
                operand,
                ty,
                nullable,
            } = file.expr(expression)
            else {
                continue;
            };
            if *nullable {
                continue;
            }
            let Expr::Name(name) = file.expr(*operand) else {
                continue;
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
        Ok(bindings)
    }
}
