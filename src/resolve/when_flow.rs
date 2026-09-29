//! Flow facts and semantic comparison plans carried between `when` conditions.

use super::scope::NarrowPath;
use super::{Checker, CheckerScope};
use crate::ast::{Expr, ExprId, WhenCondition};
use crate::types::Ty;

/// Exact numeric adaptations selected for one subject-form equality condition after earlier
/// conditions have narrowed the subject. Later phases materialize this plan without re-selecting
/// numeric applicability or promotion.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct WhenSubjectNumericEquality {
    subject_unbox: Ty,
    subject_widening: Option<Ty>,
    candidate_widening: Option<Ty>,
}

impl WhenSubjectNumericEquality {
    pub(crate) fn subject_unbox(self) -> Ty {
        self.subject_unbox
    }

    pub(crate) fn subject_widening(self) -> Option<Ty> {
        self.subject_widening
    }

    pub(crate) fn candidate_widening(self) -> Option<Ty> {
        self.candidate_widening
    }
}

impl Checker<'_> {
    /// The numeric type a name subject has in `scope` after an earlier failed type test smart-cast
    /// it. A root proof rebinds the lexical value (`declare_narrowing_shadow`); a later name read
    /// sees that binding, with a straight-line flow fact winning when one is also recorded.
    fn when_subject_numeric_type(&self, scope: &CheckerScope<'_>, subject: ExprId) -> Option<Ty> {
        let Expr::Name(name) = self.file.expr(subject) else {
            return None;
        };
        let binding = self.lookup(scope, name)?;
        let narrowed = self.local_narrowing(scope, name).unwrap_or(binding.ty);
        narrowed.is_numeric().then_some(narrowed)
    }

    /// Record the full semantic equality plan while the condition's checked candidate type and the
    /// subject's accumulated false-branch smart casts are both in scope.
    pub(super) fn record_when_subject_numeric_equality(
        &mut self,
        scope: &CheckerScope<'_>,
        condition_expression: ExprId,
        subject: Option<ExprId>,
        declared_subject: Option<Ty>,
        condition: WhenCondition,
        candidate: Ty,
    ) {
        let (Some(subject), Some(declared_subject), WhenCondition::SubjectEquals(_)) =
            (subject, declared_subject, condition)
        else {
            return;
        };
        let Some(subject_unbox) = self.when_subject_numeric_type(scope, subject) else {
            return;
        };
        if subject_unbox.canonical_semantic() == declared_subject.canonical_semantic() {
            return;
        }
        let candidate = candidate.canonical_semantic();
        if !candidate.is_numeric() {
            return;
        }
        let Some(comparison) = Ty::promote(subject_unbox, candidate) else {
            return;
        };
        self.when_subject_numeric_equalities.insert(
            condition_expression,
            WhenSubjectNumericEquality {
                subject_unbox,
                subject_widening: (comparison != subject_unbox).then_some(comparison),
                candidate_widening: (comparison != candidate).then_some(comparison),
            },
        );
    }

    /// Flow facts established by one `when` condition. Predicate conditions already contain the
    /// subject in their checked expression (`is T`, `in range`) and use ordinary condition flow.
    /// A subject-equality condition stores only the candidate expression, so null equality must be
    /// related to the separately stored subject here instead of being mistaken for a standalone
    /// Boolean condition.
    pub(super) fn when_condition_narrowings(
        &self,
        scope: &CheckerScope<'_>,
        subject: Option<ExprId>,
        condition: WhenCondition,
        truth: bool,
    ) -> (Vec<(NarrowPath, Ty)>, Vec<(String, Ty)>) {
        if let (Some(subject), WhenCondition::SubjectEquals(candidate)) = (subject, condition) {
            if matches!(self.file.expr(candidate), Expr::NullLit) {
                let mut casts = Vec::new();
                let mut declined = Vec::new();
                if truth {
                    self.null_branch_narrowings(scope, subject, &mut casts);
                } else {
                    self.null_check_narrowings(scope, subject, &mut casts, &mut declined);
                }
                return (casts, declined);
            }
            return (Vec::new(), Vec::new());
        }
        self.condition_narrowings(scope, condition.expression(), truth)
    }
}

impl super::TypeInfo {
    /// Complete numeric equality plan selected for one subject-form `when` condition.
    pub(crate) fn when_subject_numeric_equality(
        &self,
        condition: ExprId,
    ) -> Option<WhenSubjectNumericEquality> {
        self.when_subject_numeric_equalities
            .get(&condition)
            .copied()
    }
}
