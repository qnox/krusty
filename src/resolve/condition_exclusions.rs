//! Negative flow facts a condition establishes: values and classifier regions a stable access
//! path cannot denote on the edge where the condition has a given truth value. `when`
//! exhaustiveness consumes them; no complement type escapes into checked FIR.

use crate::ast::{BinOp, Expr, ExprId, UnOp};
use crate::types::Ty;

use super::scope::{FlowExclusion, NarrowPath};
use super::{Checker, CheckerScope, ExprLowering};

impl Checker<'_> {
    /// Negative value/classifier facts established when `cond` evaluates to `truth`. Only stable
    /// access paths participate; a computed property or mutable captured value may change before a
    /// later `when` and therefore cannot make that `when` exhaustive.
    pub(super) fn condition_exclusions(
        &self,
        scope: &CheckerScope<'_>,
        cond: ExprId,
        truth: bool,
    ) -> Vec<(NarrowPath, FlowExclusion)> {
        if let Expr::Unary {
            op: UnOp::Not,
            operand,
        } = self.file.expr(cond).clone()
        {
            return self.condition_exclusions(scope, operand, !truth);
        }
        if let Expr::Binary { op, lhs, rhs, .. } = self.file.expr(cond).clone() {
            let combines = (truth && op == BinOp::And) || (!truth && op == BinOp::Or);
            if combines {
                let mut exclusions = self.condition_exclusions(scope, lhs, truth);
                exclusions.extend(self.condition_exclusions(scope, rhs, truth));
                return exclusions;
            }
            // Equality against a Boolean literal is a truth projection, even when the other
            // operand is itself a condition. For example, normal continuation after
            // `if ((b == true) == false) return` proves `(b == true)`, hence `b` is neither false
            // nor null. Keep this recursive and semantic instead of recognizing that source
            // spelling: every checked Boolean expression can receive the projected truth value.
            if matches!(op, BinOp::Eq | BinOp::Ne) {
                for (condition, literal) in [(lhs, rhs), (rhs, lhs)] {
                    let Expr::BoolLit(literal) = self.file.expr(literal) else {
                        continue;
                    };
                    if self.expr_types[condition.0 as usize].non_null() != Ty::Boolean {
                        continue;
                    }
                    let condition_truth = if (op == BinOp::Eq) == truth {
                        *literal
                    } else {
                        !*literal
                    };
                    return self.boolean_value_exclusions(scope, condition, condition_truth);
                }
            }
            let excludes_equality = (op == BinOp::Eq && !truth) || (op == BinOp::Ne && truth);
            if excludes_equality {
                for (path_expression, value_expression) in [(lhs, rhs), (rhs, lhs)] {
                    let Some(path) = self.expr_access_path(path_expression) else {
                        continue;
                    };
                    if self
                        .stable_path_ty(scope, &path, self.span(path_expression))
                        .is_none()
                    {
                        continue;
                    }
                    if let Some(exclusion) = self.flow_exclusion_value(value_expression) {
                        return vec![(path, exclusion)];
                    }
                }
            }
            return Vec::new();
        }
        if let Expr::Is {
            operand,
            ty,
            negated,
        } = self.file.expr(cond).clone()
        {
            if negated != truth {
                return Vec::new();
            }
            let Some(path) = self.expr_access_path(operand) else {
                return Vec::new();
            };
            if self
                .stable_path_ty(scope, &path, self.span(operand))
                .is_none()
            {
                return Vec::new();
            }
            let resolved = self
                .resolved_type_tys
                .get(&(ty.span.lo, ty.span.hi))
                .copied()
                .unwrap_or_else(|| self.type_ref_ty_silent(scope, &ty));
            return resolved
                .non_null()
                .obj_internal()
                .map(|classifier| (path, FlowExclusion::Classifier(classifier)))
                .into_iter()
                .collect();
        }
        let Some(path) = self.expr_access_path(cond) else {
            return Vec::new();
        };
        if self.expr_types[cond.0 as usize] != Ty::Boolean
            || self.stable_path_ty(scope, &path, self.span(cond)).is_none()
        {
            return Vec::new();
        }
        let mut exclusions = vec![(path, FlowExclusion::Boolean(!truth))];
        exclusions.extend(self.implied_condition_exclusions(scope, cond, truth));
        exclusions
    }

    /// Facts implied by a checked Boolean expression having the exact value `truth`. Equality can
    /// establish such a value for a nullable Boolean path even though that path cannot itself be
    /// used as an `if` condition; the successful comparison also excludes null.
    fn boolean_value_exclusions(
        &self,
        scope: &CheckerScope<'_>,
        expression: ExprId,
        truth: bool,
    ) -> Vec<(NarrowPath, FlowExclusion)> {
        if let Some(path) = self.expr_access_path(expression) {
            let ty = self.expr_types[expression.0 as usize];
            if ty.non_null() == Ty::Boolean
                && self
                    .stable_path_ty(scope, &path, self.span(expression))
                    .is_some()
            {
                let mut exclusions = vec![(path.clone(), FlowExclusion::Boolean(!truth))];
                if ty.is_nullable() {
                    exclusions.push((path, FlowExclusion::Null));
                }
                exclusions.extend(self.implied_condition_exclusions(scope, expression, truth));
                return exclusions;
            }
        }
        self.condition_exclusions(scope, expression, truth)
    }

    fn flow_exclusion_value(&self, expression: ExprId) -> Option<FlowExclusion> {
        if let Some(entry) = self.resolved_enum_entries.get(&expression) {
            return Some(FlowExclusion::EnumEntry {
                classifier: entry.classifier,
                name: entry.name.clone(),
            });
        }
        match self.file.expr(expression) {
            Expr::BoolLit(value) => Some(FlowExclusion::Boolean(*value)),
            Expr::NullLit => Some(FlowExclusion::Null),
            _ => match self.expr_lowers.get(&expression) {
                Some(ExprLowering::SingletonValue(singleton)) => {
                    Some(FlowExclusion::Singleton(singleton.classifier))
                }
                _ => None,
            },
        }
    }

    pub(super) fn apply_condition_exclusions(
        &self,
        scope: &CheckerScope<'_>,
        cond: ExprId,
        truth: bool,
    ) {
        for (path, exclusion) in self.condition_exclusions(scope, cond, truth) {
            scope.exclude(path, exclusion);
        }
    }
}
