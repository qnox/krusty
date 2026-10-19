//! Elvis-expression typing, sibling completion, and checked result-value publication.

use super::*;

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
                // must fit the enclosing expectation, but the expression itself may return null;
                // therefore `String` contextually types the left as `String?`. Passing the exact
                // non-null expectation into a generic call prematurely constrained its result and
                // rejected valid shapes such as `nullable.let(::nullableResult) ?: fallback`.
                Some(ty) => self.expr_expected(scope, lhs, Ty::nullable(ty)),
                None => self.expr(scope, lhs),
            };
            // The non-null LHS constrains an otherwise underdetermined generic fallback:
            // `map[key] ?: emptyList()` must infer the element type from `map[key]`, just as the
            // same generic call is retyped from an enclosing callable parameter elsewhere.
            let mut lt = definitely_non_null_ty(lt0);
            // The right operand runs only when the left is null, so a cast or assignment it proves
            // must not become a fact of the value that took the left operand.
            let rhs_scope = scope.child(ScopeKind::Block);
            let rt = match conditional_expected {
                Some(ty) => self.expr_expected(&rhs_scope, rhs, ty),
                // The bottom type contributes no constraint to the other branch. In particular,
                // `null ?: run { statement }` must infer `run` from its lambda as `Unit`; forcing
                // the non-null form of `Null` (`Nothing`) into the RHS instead asks the lambda to
                // return `Nothing` and rejects every normally-completing statement.
                None if lt == Ty::Nothing || self.constructor_branch_is_open(lhs) => {
                    self.expr(&rhs_scope, rhs)
                }
                None => self.expr_expected(&rhs_scope, rhs, lt),
            };
            let fixed = self.expectation_fixes_branches(scope, e, conditional_expected);
            let rhs_evidence = self.branch_sibling_type(rhs, rt);
            let lt0 = self.rebind_conditional_branch(lhs, rhs_evidence, lt0, fixed, |c, exp| {
                c.expr_expected(scope, lhs, exp)
            });
            lt = definitely_non_null_ty(lt0);
            let lhs_evidence = self.branch_sibling_type(lhs, lt);
            let rt = self.rebind_conditional_branch(rhs, lhs_evidence, rt, fixed, |c, exp| {
                c.expr_expected(&rhs_scope, rhs, exp)
            });
            // The elvis value when lhs is non-null: a nullable-primitive lhs (`Int?`) unwraps to its
            // unboxed primitive, while an ordinary nullable reference `T?` unwraps to `T`.
            crate::trace_compiler!(
                "resolve",
                "elvis expression={e:?} lhs={lt0:?} non_null_lhs={lt:?} rhs={rt:?} expected={expected:?}"
            );
            let result = if rt == Ty::Nothing {
                // `x ?: return` (or throw/break/continue/a `Nothing` call): completing the elvis
                // proves a stable `x` non-null for the code that follows, exactly like an
                // `if (x == null) return` guard — the same machinery (`null_check_narrowings`)
                // covers plain names, property paths (`a.b ?: return` narrows `a.b`), and
                // SAFE-CALL left sides (`u?.f ?: return` proves the root `u` non-null).
                // Unsigned stays unnarrowed because its value-box unbox to `kotlin.UInt` is
                // not modeled.
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
                self.publish_conditional_intersection(e, [(lhs, result)], result);
                result
            } else if lt == Ty::Null || matches!(lt0, Ty::Nullable(inner) if *inner == Ty::Nothing)
            {
                self.record_flexible_alias(e, rhs, false);
                self.publish_conditional_intersection(e, [(rhs, rt)], rt);
                rt
            } else {
                let joined =
                    conditional_branch::join_types(self, scope, conditional_expected, lt, rt, e);
                self.record_flexible_join(e, [lhs, rhs], joined, true);
                self.publish_conditional_intersection(e, [(lhs, lt), (rhs, rt)], joined);
                joined
            };
            self.complete_conditional_branches(
                scope,
                e,
                conditional_expected,
                result,
                [(lhs, lt), (rhs, rt)],
            );
            result
        };
        self.set(e, t)
    }
}
