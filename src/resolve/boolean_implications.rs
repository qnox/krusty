//! Data-flow implications bound to a stable local `Boolean` `val`.
//!
//! Kotlin binds the type statements a condition proves to the stable Boolean variable holding the
//! condition's value: after `val ok = x != null`, `if (ok)` proves `x != null` exactly as
//! `if (x != null)` would, and a contract applies through the variable the same way
//! (`val empty = s.isNullOrEmpty()` proves `s != null` when `empty` is false).
//!
//! The facts are computed once, where the initializer is checked, and recorded on the flow frame
//! that declares the val. A fact the stability gate declines there is no fact at all. A write in
//! the val's own execution to a value a fact talks about drops that fact; a write in a nested
//! closure does not, because every fact passes the stability gate again where the val is read as
//! a condition, and a closure that mutates the value in between declines the cast there
//! (SMARTCAST_IMPOSSIBLE).

use crate::ast::{Expr, ExprId};
use crate::types::Ty;

use super::scope::{
    BooleanImplications, ConditionFacts, FlowExclusion, FlowExecutionIdentity, NarrowPath, PathRoot,
};
use super::{Checker, CheckerScope};

impl Checker<'_> {
    fn loop_may_replace_implication_root(&self, path: &NarrowPath) -> bool {
        let PathRoot::Value(identity) = &path.root else {
            return false;
        };
        self.provisional_loop_compound_writes
            .iter()
            .rev()
            .any(|writes| writes.contains_key(identity))
    }

    /// One syntactic compound assignment has now gone through ordinary operator selection. Drop
    /// that pending decision from every enclosing loop's back-edge summary. A later compound write
    /// to the same root remains pending through its own count; a replacement write separately
    /// removes the implication when [`Checker::stmt_assign`] commits it.
    pub(super) fn resolve_provisional_loop_compound_write(&mut self, name: &str, value: ExprId) {
        if self.file.value_operator_spans.contains_key(&value.0) {
            return;
        }
        let Expr::Binary { lhs, .. } = self.file.expr(value) else {
            return;
        };
        if !matches!(self.file.expr(*lhs), Expr::Name(lhs) if lhs == name) {
            return;
        }
        let Some(path) = self.expr_access_path(*lhs) else {
            return;
        };
        let PathRoot::Value(identity) = path.root else {
            return;
        };
        for writes in &mut self.provisional_loop_compound_writes {
            let Some(remaining) = writes.get_mut(&identity) else {
                continue;
            };
            *remaining -= 1;
            if *remaining == 0 {
                writes.remove(&identity);
            }
        }
    }

    /// The implications of a stable local `val` initialized by `init`, computed in the scope the
    /// initializer was checked in. `None` when the val is not a Boolean or implies nothing.
    pub(super) fn boolean_val_implications(
        &self,
        scope: &CheckerScope<'_>,
        init: ExprId,
        bound_ty: Ty,
    ) -> Option<BooleanImplications> {
        if bound_ty != Ty::Boolean {
            return None;
        }
        let facts = |truth| ConditionFacts {
            narrowings: self.condition_narrowings(scope, init, truth).0,
            exclusions: self.condition_exclusions(scope, init, truth),
        };
        let implications = BooleanImplications {
            when_true: facts(true),
            when_false: facts(false),
            execution: self.source_execution(),
        };
        (!implications.when_true.is_empty() || !implications.when_false.is_empty())
            .then_some(implications)
    }

    /// The implications recorded for a condition that reads a stable Boolean `val` directly.
    fn read_boolean_implications(
        &self,
        scope: &CheckerScope<'_>,
        condition: ExprId,
    ) -> Option<BooleanImplications> {
        if !matches!(self.file.expr(condition), Expr::Name(_)) {
            return None;
        }
        let path = self.expr_access_path(condition)?;
        let (PathRoot::Value(identity), true) = (&path.root, path.segments.is_empty()) else {
            return None;
        };
        scope.boolean_implications(*identity)
    }

    /// Narrowings a stable Boolean `val` read as `condition` proves when it is `truth`. Each fact
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
        let Some(implications) = self.read_boolean_implications(scope, condition) else {
            return;
        };
        let site = self.span(condition);
        for (path, ty) in &implications.facts(truth).narrowings {
            if !self.loop_may_replace_implication_root(path)
                && self.is_proof_stable_or_decline(scope, path, *ty, declined, site)
            {
                out.push((path.clone(), *ty));
            }
        }
    }

    /// Negative facts a stable Boolean `val` read as `condition` proves when it is `truth`.
    pub(super) fn implied_condition_exclusions(
        &self,
        scope: &CheckerScope<'_>,
        condition: ExprId,
        truth: bool,
    ) -> Vec<(NarrowPath, FlowExclusion)> {
        let Some(implications) = self.read_boolean_implications(scope, condition) else {
            return Vec::new();
        };
        let site = self.span(condition);
        implications
            .facts(truth)
            .exclusions
            .iter()
            .filter(|(path, _)| {
                !self.loop_may_replace_implication_root(path)
                    && self.stable_path_ty(scope, path, site).is_some()
            })
            .cloned()
            .collect()
    }

    /// A write to the value with flow identity `identity` replaced it: drop what the Boolean vals
    /// declared in the current execution said about it.
    pub(super) fn forget_implied_facts(&self, scope: &CheckerScope<'_>, identity: u32) {
        scope.forget_implied_facts_rooted_at(
            &PathRoot::Value(identity),
            &self.source_execution(),
            |lambda| self.lambda_is_inline_spliced(lambda),
        );
    }

    /// The source lambdas enclosing the expression being checked, outermost first. Inline capture
    /// selection can finish after a Boolean val is declared, so identities retain the full chain
    /// and normalize both sides only when a write invalidates facts.
    fn source_execution(&self) -> FlowExecutionIdentity {
        FlowExecutionIdentity {
            local_function: self.local_function_execution,
            lambdas: self.lambda_returns.active_chain().to_vec(),
        }
    }
}
