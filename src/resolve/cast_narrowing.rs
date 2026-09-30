//! Casts that have already run, for a later operand of the same expression.
//!
//! An eager operator evaluates its left operand completely before the right, so a non-null `as`
//! there has run. `&&` and `||` do not: their right operands can be skipped.

use crate::ast::{BinOp, Expr, ExprId};
use crate::types::Ty;

use super::scope::{NarrowPath, ScopeKind};
use super::{Checker, CheckerScope};

impl Checker<'_> {
    /// Collect checked-cast facts from subexpressions that certainly ran when `expression` ran.
    pub(super) fn as_cast_narrowings(
        &self,
        scope: &CheckerScope<'_>,
        expression: ExprId,
        out: &mut Vec<(NarrowPath, Ty)>,
    ) {
        match self.file.expr(expression).clone() {
            Expr::As {
                operand,
                ty,
                nullable,
            } => {
                self.as_cast_narrowings(scope, operand, out);
                if nullable {
                    return;
                }
                let Some(path) = self.expr_access_path(operand) else {
                    return;
                };
                let Some(stable_ty) = self.stable_path_ty(scope, &path, self.span(expression))
                else {
                    return;
                };
                if let Some(narrowed) = self.proven_narrowed_ty(scope, Some(stable_ty), &ty) {
                    out.push((path, narrowed));
                }
            }
            Expr::Binary { op, lhs, rhs, .. } => {
                self.as_cast_narrowings(scope, lhs, out);
                if !matches!(op, BinOp::And | BinOp::Or) {
                    self.as_cast_narrowings(scope, rhs, out);
                }
            }
            Expr::Elvis { lhs, .. } => self.as_cast_narrowings(scope, lhs, out),
            Expr::Unary { operand, .. } | Expr::NotNull { operand } | Expr::Is { operand, .. } => {
                self.as_cast_narrowings(scope, operand, out)
            }
            Expr::Member { receiver, .. } | Expr::SafeCall { receiver, .. } => {
                self.as_cast_narrowings(scope, receiver, out)
            }
            Expr::Index { array, indices } => {
                self.as_cast_narrowings(scope, array, out);
                for index in indices {
                    self.as_cast_narrowings(scope, index, out);
                }
            }
            Expr::Call { callee, args } => {
                self.as_cast_narrowings(scope, callee, out);
                for argument in args {
                    self.as_cast_narrowings(scope, argument, out);
                }
            }
            Expr::InRange {
                value, start, end, ..
            } => {
                self.as_cast_narrowings(scope, value, out);
                self.as_cast_narrowings(scope, start, out);
                self.as_cast_narrowings(scope, end, out);
            }
            Expr::RangeTo { lo, hi, .. } => {
                self.as_cast_narrowings(scope, lo, out);
                self.as_cast_narrowings(scope, hi, out);
            }
            Expr::If { cond, .. } => self.as_cast_narrowings(scope, cond, out),
            _ => {}
        }
    }

    /// Scope for the right operand of an eager operator. `None` when the left operand ran no
    /// non-null `as`.
    pub(super) fn evaluated_cast_scope<'a>(
        &mut self,
        scope: &'a CheckerScope<'_>,
        lhs: ExprId,
    ) -> Option<CheckerScope<'a>> {
        let mut casts = Vec::new();
        self.as_cast_narrowings(scope, lhs, &mut casts);
        if casts.is_empty() {
            return None;
        }
        let rhs_scope = scope.child(ScopeKind::Block);
        self.apply_narrowings(&rhs_scope, &casts, &[], false);
        Some(rhs_scope)
    }
}
