//! Casts that have already run, for a later operand of the same expression.
//!
//! An eager operator evaluates its left operand completely before the right, so a non-null `as`
//! there has run. `&&` and `||` do not: their right operands can be skipped.

use crate::ast::{Expr, ExprId, StmtId};
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
        let mut evaluated = Vec::new();
        crate::ast::definitely_evaluated::for_each_in_expression(
            self.file,
            expression,
            &mut |expression| evaluated.push(expression),
        );
        for expression in evaluated {
            self.append_as_cast_narrowing(scope, expression, out);
        }
    }

    /// Checked-cast facts established by every normally completing path through `statement`.
    pub(super) fn as_cast_narrowings_after_statement(
        &self,
        scope: &CheckerScope<'_>,
        statement: StmtId,
        out: &mut Vec<(NarrowPath, Ty)>,
    ) {
        let mut evaluated = Vec::new();
        crate::ast::definitely_evaluated::for_each_in_statement(
            self.file,
            statement,
            &mut |expression| evaluated.push(expression),
        );
        for expression in evaluated {
            self.append_as_cast_narrowing(scope, expression, out);
        }
    }

    fn append_as_cast_narrowing(
        &self,
        scope: &CheckerScope<'_>,
        expression: ExprId,
        out: &mut Vec<(NarrowPath, Ty)>,
    ) {
        let Expr::As {
            operand,
            ty,
            nullable: false,
        } = self.file.expr(expression)
        else {
            return;
        };
        let Some(path) = self.expr_access_path(*operand) else {
            return;
        };
        let Some(stable_ty) = self.stable_path_ty(scope, &path, self.span(expression)) else {
            return;
        };
        if let Some(narrowed) = self.proven_narrowed_ty(scope, Some(stable_ty), ty) {
            out.push((path, narrowed));
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
