//! Data-flow implications bound to a stable value.
//!
//! Kotlin binds the type statements a condition proves to the stable variable holding the
//! condition's value: after `val ok = x != null`, `if (ok)` proves `x != null` exactly as
//! `if (x != null)` would, and a contract applies through the variable the same way
//! (`val empty = s.isNullOrEmpty()` proves `s != null` when `empty` is false). A stable value
//! holding a call's result carries the call's `returnsNotNull()` and `returns(null)` effects the
//! same way: after `val selector = selectorFor(channel)`, or a `val` property initialized so in
//! `init`, `if (selector != null)` proves what `if (selectorFor(channel) != null)` would, and after
//! `val result = origin?.selector` a non-null `result` proves `origin` non-null. Every null proof
//! of such a value applies them (`safe_call_flow`).
//!
//! The facts are computed once, where the value is bound, and recorded on the flow frame that
//! binds it. A fact the stability gate declines there is no fact at all. A write in the value's
//! own execution to a value a fact talks about drops that fact; a write in a nested closure does
//! not, because every fact passes the stability gate again where the value is read as a
//! condition, and a closure that mutates the value in between declines the cast there
//! (SMARTCAST_IMPOSSIBLE).

use crate::ast::{Expr, ExprId};
use crate::contracts::ReturnsValue;
use crate::types::Ty;

use super::scope::{
    ConditionFacts, FlowExclusion, FlowExecutionIdentity, ImplyingQuestion, NarrowPath, PathRoot,
    ValueImplications,
};
use super::{Checker, CheckerScope};

impl Checker<'_> {
    /// The implications of a stable value bound to `init`, computed in the scope the initializer
    /// was checked in: a `Boolean` by the facts its condition proves, a nullable value by the
    /// effects the contract of the call it holds proves for a null and a non-null result. `None`
    /// when the value implies nothing.
    pub(super) fn value_implications(
        &self,
        scope: &CheckerScope<'_>,
        init: ExprId,
        bound_ty: Ty,
    ) -> Option<ValueImplications> {
        let (question, facts): (_, &dyn Fn(bool) -> ConditionFacts) = if bound_ty == Ty::Boolean {
            (ImplyingQuestion::Truth, &|truth| ConditionFacts {
                narrowings: self.condition_narrowings(scope, init, truth).0,
                exclusions: self.condition_exclusions(scope, init, truth),
            })
        } else if bound_ty.is_nullable() {
            (ImplyingQuestion::NonNull, &|non_null| {
                let mut narrowings = Vec::new();
                let mut declined = Vec::new();
                let result = if non_null {
                    ReturnsValue::NotNull
                } else {
                    ReturnsValue::Null
                };
                self.contract_result_narrowings(
                    scope,
                    init,
                    result,
                    &mut narrowings,
                    &mut declined,
                );
                // A non-null safe-call result proves every `?.` of its chain held.
                if non_null && matches!(self.file.expr(init), Expr::SafeCall { .. }) {
                    self.null_check_narrowings(scope, init, &mut narrowings, &mut declined);
                }
                ConditionFacts {
                    narrowings,
                    exclusions: Vec::new(),
                }
            })
        } else {
            return None;
        };
        let implications = ValueImplications {
            question,
            when_true: facts(true),
            when_false: facts(false),
            execution: self.source_execution(),
        };
        (!implications.when_true.is_empty() || !implications.when_false.is_empty())
            .then_some(implications)
    }

    /// The implications recorded for the `Boolean` stable value `condition` reads. A value compared
    /// with `null` applies its implications through the null proof itself
    /// (`null_proof_or_decline`, `implied_null_narrowings`).
    fn read_value_implications(
        &self,
        scope: &CheckerScope<'_>,
        condition: ExprId,
    ) -> Option<ValueImplications> {
        if !matches!(self.file.expr(condition), Expr::Name(_)) {
            return None;
        }
        let path = self.expr_access_path(condition)?;
        scope
            .value_implications(&path)
            .filter(|implications| implications.question == ImplyingQuestion::Truth)
    }

    /// Narrowings a stable value read as `condition` proves when it is `truth`. Each fact
    /// passes the stability gate at this read: a root a capturing closure mutates is declined here
    /// even when it was stable where the val was declared.
    pub(super) fn implied_condition_narrowings(
        &self,
        scope: &CheckerScope<'_>,
        condition: ExprId,
        truth: bool,
        out: &mut Vec<(NarrowPath, Ty)>,
        declined: &mut Vec<(String, Ty)>,
    ) {
        let Some(implications) = self.read_value_implications(scope, condition) else {
            return;
        };
        let site = self.span(condition);
        for (path, ty) in &implications.facts(truth).narrowings {
            if self.is_proof_stable_or_decline(scope, path, *ty, declined, site) {
                out.push((path.clone(), *ty));
            }
        }
    }

    /// Negative facts a stable value read as `condition` proves when it is `truth`.
    pub(super) fn implied_condition_exclusions(
        &self,
        scope: &CheckerScope<'_>,
        condition: ExprId,
        truth: bool,
    ) -> Vec<(NarrowPath, FlowExclusion)> {
        let Some(implications) = self.read_value_implications(scope, condition) else {
            return Vec::new();
        };
        let site = self.span(condition);
        implications
            .facts(truth)
            .exclusions
            .iter()
            .filter(|(path, _)| self.stable_path_ty(scope, path, site).is_some())
            .cloned()
            .collect()
    }

    /// A write to the value with flow identity `identity` replaced it: drop what the stable values
    /// bound in the current execution said about it.
    pub(super) fn forget_implied_facts(&self, scope: &CheckerScope<'_>, identity: u32) {
        scope.forget_implied_facts_rooted_at(
            &PathRoot::Value(identity),
            &self.source_execution(),
            |lambda| self.lambda_is_inline_spliced(lambda),
        );
    }

    /// The source lambdas enclosing the expression being checked, outermost first. Inline capture
    /// selection can finish after a stable value is bound, so identities retain the full chain
    /// and normalize both sides only when a write invalidates facts.
    fn source_execution(&self) -> FlowExecutionIdentity {
        FlowExecutionIdentity {
            local_function: self.local_function_execution,
            lambdas: self.lambda_returns.active_chain().to_vec(),
        }
    }
}
