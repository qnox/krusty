//! Elvis-expression checking and checked result-value publication.

use crate::ast::ExprId;
use crate::types::Ty;

use super::{conditional_branch, definitely_non_null_ty, Checker, CheckerScope, ScopeKind};

impl Checker<'_> {
    pub(super) fn expr_inner_elvis(
        &mut self,
        scope: &CheckerScope<'_>,
        e: ExprId,
        expected: Option<Ty>,
        lhs: ExprId,
        rhs: ExprId,
    ) -> Ty {
        let t = {
            let conditional_expected = conditional_branch::usable_expected(scope, expected);
            let lt0 = match conditional_expected {
                // The left branch is observed only after an implicit null test. Its NON-null value
                // must fit the enclosing expectation, but the expression itself may return null.
                Some(ty) => self.expr_expected(scope, lhs, Ty::nullable(ty)),
                None => self.expr(scope, lhs),
            };
            let mut lt = definitely_non_null_ty(lt0);
            // The right operand runs only when the left is null, so its flow facts stay in a child.
            let rhs_scope = scope.child(ScopeKind::Block);
            let rt = match conditional_expected {
                Some(ty) => self.expr_expected(&rhs_scope, rhs, ty),
                None if lt == Ty::Nothing => self.expr(&rhs_scope, rhs),
                None => self.expr_expected(&rhs_scope, rhs, lt),
            };
            let fixed = self.expectation_fixes_branches(scope, e, conditional_expected);
            let lt0 = self.rebind_conditional_branch(lhs, rt, lt0, fixed, |c, exp| {
                c.expr_expected(scope, lhs, exp)
            });
            lt = definitely_non_null_ty(lt0);
            let rt = self.rebind_conditional_branch(rhs, lt, rt, fixed, |c, exp| {
                c.expr_expected(&rhs_scope, rhs, exp)
            });
            self.report_unbound_conditional_branch(scope, lhs);
            self.report_unbound_conditional_branch(scope, rhs);
            crate::trace_compiler!(
                "resolve",
                "elvis expression={e:?} lhs={lt0:?} non_null_lhs={lt:?} rhs={rt:?} expected={expected:?}"
            );
            if rt == Ty::Nothing {
                // Normal completion proves the left access path non-null.
                let mut proven = Vec::new();
                let mut declined = Vec::new();
                self.null_check_narrowings(scope, lhs, &mut proven, &mut declined);
                proven.retain(|(_, ty)| !ty.is_unsigned());
                self.apply_narrowings(scope, &proven, &declined, false);
                let result = match lt0 {
                    Ty::Nullable(inner) => *inner,
                    _ => lt,
                };
                self.record_flexible_alias(e, lhs, true);
                result
            } else if lt == Ty::Null || matches!(lt0, Ty::Nullable(inner) if *inner == Ty::Nothing)
            {
                self.record_flexible_alias(e, rhs, false);
                rt
            } else {
                let joined =
                    conditional_branch::join_types(self, scope, conditional_expected, lt, rt, e);
                self.record_flexible_join(e, [lhs, rhs], joined, true);
                joined
            }
        };
        self.set(e, t)
    }
}
