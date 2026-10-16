//! Compact flow effects of simple-name assignments and augmented assignments.

use crate::ast::{BinOp, Expr, ExprId, File};
use crate::fir::SigBinaryOperator;

use super::{
    ExpressionForm, OriginId, SigExpr, SigExprId, SignatureConstraintExtractor, SignatureScopeId,
};

impl SignatureConstraintExtractor {
    fn lexical_operator_function_value(&self, spelling: &str) -> Option<SigExprId> {
        self.lexical_callables.iter().rev().find_map(|values| {
            values
                .get(spelling)
                .filter(|callable| callable.has_receiver && callable.operator)
                .map(|callable| callable.value)
        })
    }

    pub(super) fn simple_name_assignment(
        &mut self,
        file: &File,
        name: &str,
        value: ExprId,
        scope: SignatureScopeId,
        origin: &mut impl FnMut(crate::diag::Span) -> OriginId,
    ) -> Result<SigExprId, ExpressionForm> {
        let effect = self.expression(file, value, scope, origin)?;
        let is_compound = !file.value_operator_spans.contains_key(&value.0)
            && matches!(
                file.expr(value),
                Expr::Binary {
                    op: BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Rem,
                    lhs,
                    ..
                } if matches!(file.expr(*lhs), Expr::Name(lhs) if lhs == name)
            );
        let deferred = if is_compound {
            match self.graph.expr(effect) {
                Some(SigExpr::Binary {
                    lhs, rhs, operator, ..
                }) => Some((
                    lhs,
                    rhs,
                    operator,
                    origin(file.expr_spans[value.0 as usize]),
                )),
                _ => None,
            }
        } else {
            None
        };
        match deferred {
            Some((receiver, argument, operator, statement_origin)) => {
                let spelling = match operator {
                    SigBinaryOperator::Add => "plusAssign",
                    SigBinaryOperator::Subtract => "minusAssign",
                    SigBinaryOperator::Multiply => "timesAssign",
                    SigBinaryOperator::Divide => "divAssign",
                    SigBinaryOperator::Remainder => "remAssign",
                    _ => unreachable!("compound extraction admits only arithmetic operators"),
                };
                let local_operator = self.lexical_operator_function_value(spelling);
                self.defer_compound_boolean_val_invalidation(
                    name,
                    receiver,
                    argument,
                    operator,
                    local_operator,
                    scope,
                    statement_origin,
                )
            }
            None => self.forget_boolean_vals_depending_on(name),
        }
        Ok(effect)
    }
}
