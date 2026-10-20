//! Flow facts and semantic comparison plans carried between `when` conditions.

use super::scope::NarrowPath;
use super::{Checker, CheckerScope};
use crate::ast::{Expr, ExprId, WhenCondition};
use crate::fx_hash::HashMap;
use crate::types::Ty;

/// A condition's narrowings: the casts to apply, and the ones a closure mutation declines, keyed
/// by the declined path's spelling.
type WhenNarrowings = (Vec<(NarrowPath, Ty)>, Vec<(String, Ty)>);

/// The subject facts each condition of a subject `when` was checked against, keyed by the
/// condition expression.
#[derive(Default)]
pub(crate) struct WhenSubjectPlans {
    /// Complete numeric equality plans for subject-form conditions after earlier failed type
    /// tests.
    numeric_equalities: HashMap<ExprId, WhenSubjectNumericEquality>,
    /// The narrowed view of the subject an `is`/`in` condition reads, where earlier conditions
    /// narrowed it below its own type.
    condition_views: HashMap<ExprId, Ty>,
    /// Subject-form equality conditions that are fir2ir's `ieee754equals` of the recorded
    /// floating type with a nullable operand.
    nullable_ieee754_equalities: HashMap<ExprId, Ty>,
}

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

    /// Record what one condition of a subject `when` was checked against, while the condition's
    /// checked type and the subject's accumulated false-branch smart casts are both in scope.
    pub(super) fn record_when_condition_subject_plan(
        &mut self,
        scope: &CheckerScope<'_>,
        condition_expression: ExprId,
        subject: Option<ExprId>,
        declared_subject: Option<Ty>,
        condition: WhenCondition,
        checked: Ty,
    ) {
        let (Some(subject), Some(declared_subject)) = (subject, declared_subject) else {
            return;
        };
        if condition.is_predicate() {
            self.record_when_subject_condition_view(
                condition_expression,
                subject,
                declared_subject,
            );
        } else {
            self.record_when_subject_numeric_equality(
                scope,
                condition_expression,
                subject,
                declared_subject,
                checked,
            );
        }
    }

    /// An `is`/`in` condition holds the subject's own syntax node as its operand, and checking
    /// the condition in its arm's flow typed that node as the arm sees the subject. The node is
    /// still the `when`'s subject, evaluated once at entry with its own type, as kotlinc's
    /// `tmp_subject` is. Record the arm's view against the condition, which reads the subject
    /// through it as kotlinc's smart-cast `$subj$` does, and keep the subject's own type on the
    /// node. Otherwise the subject would be converted to a later arm's view before any arm ran:
    /// a nullable primitive narrowed past a `null` arm was unboxed up front, and the `null` arm
    /// compared a primitive with `null`.
    fn record_when_subject_condition_view(
        &mut self,
        condition_expression: ExprId,
        subject: ExprId,
        declared_subject: Ty,
    ) {
        let view = self.expr_types[subject.0 as usize];
        if view == declared_subject || view == Ty::Error {
            return;
        }
        self.when_subject_plans
            .condition_views
            .insert(condition_expression, view);
        self.set(subject, declared_subject);
    }

    /// The full semantic equality plan of a subject-form equality condition.
    ///
    /// kotlinc compares `$subj$ == candidate` as fir2ir converts any `==` operand
    /// (`OperatorExpressionGenerator.convertToIrExpression`): when the smart-cast subject and the
    /// candidate have one primitive numeric comparison type, the smart cast is erased and the
    /// subject is compared as read, so a boxed `Int?` subject meets an `Int` candidate in kotlinc's
    /// `PrimitiveToObjectComparison` (the candidate stored first, then the subject unboxed). A
    /// `Float`/`Double` keeps its cast, as a nullable one when the subject was nullable, which is
    /// again the subject as read. Otherwise the subject is unboxed and each operand converted to
    /// the comparison type. A floating comparison type makes the equality fir2ir's
    /// `ieee754equals`, whose nullable operands keep their boxes.
    fn record_when_subject_numeric_equality(
        &mut self,
        scope: &CheckerScope<'_>,
        condition_expression: ExprId,
        subject: ExprId,
        declared_subject: Ty,
        candidate: Ty,
    ) {
        let declared_subject = declared_subject.canonical_semantic();
        let candidate = candidate.canonical_semantic();
        // A nullable candidate keeps its own type; only a non-null one is converted.
        let nullable_candidate = candidate.nullable_primitive();
        let candidate_value = nullable_candidate.unwrap_or(candidate);
        if !candidate_value.is_numeric() {
            return;
        }
        let narrowed = self
            .when_subject_numeric_type(scope, subject)
            .filter(|narrowed| narrowed.canonical_semantic() != declared_subject);
        let Some(subject_value) = narrowed.or_else(|| {
            Some(
                declared_subject
                    .nullable_primitive()
                    .unwrap_or(declared_subject),
            )
            .filter(|ty| ty.is_numeric())
        }) else {
            return;
        };
        let Some(comparison) = Ty::promote(subject_value, candidate_value) else {
            return;
        };
        if nullable_candidate.is_some() && comparison != candidate_value {
            return;
        }
        let floating = matches!(comparison, Ty::Float | Ty::Double);
        let compares_subject_as_read = narrowed.is_none_or(|narrowed| {
            candidate == narrowed
                && comparison == narrowed
                && (!floating || declared_subject == Ty::nullable(narrowed))
        });
        if let Some(subject_unbox) = narrowed.filter(|_| !compares_subject_as_read) {
            self.when_subject_plans.numeric_equalities.insert(
                condition_expression,
                WhenSubjectNumericEquality {
                    subject_unbox,
                    subject_widening: (comparison != subject_unbox).then_some(comparison),
                    candidate_widening: (nullable_candidate.is_none() && comparison != candidate)
                        .then_some(comparison),
                },
            );
        }
        let boxed_subject =
            compares_subject_as_read && declared_subject.nullable_primitive().is_some();
        if floating && (boxed_subject || nullable_candidate.is_some()) {
            self.when_subject_plans
                .nullable_ieee754_equalities
                .insert(condition_expression, comparison);
        }
    }

    /// Flow facts established by one `when` condition. Predicate conditions already contain the
    /// subject in their checked expression (`is T`, `in range`) and use ordinary condition flow.
    /// A subject-equality condition stores only the candidate expression, so null equality must be
    /// related to the separately stored subject here instead of being mistaken for a standalone
    /// Boolean condition.
    ///
    /// Returns the narrowings to apply and the ones declined.
    pub(super) fn when_condition_narrowings(
        &self,
        scope: &CheckerScope<'_>,
        subject: Option<ExprId>,
        condition: WhenCondition,
        truth: bool,
    ) -> WhenNarrowings {
        if let (Some(subject), WhenCondition::SubjectEquals(candidate)) = (subject, condition) {
            if let Expr::BoolLit(candidate) = self.file.expr(candidate) {
                let subject_truth = if truth { *candidate } else { !*candidate };
                return self.condition_narrowings(scope, subject, subject_truth);
            }
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
        self.when_subject_plans
            .numeric_equalities
            .get(&condition)
            .copied()
    }

    /// The floating type of a subject-form equality condition that is IEEE-754 equality with a
    /// nullable operand.
    pub(crate) fn when_subject_nullable_ieee754_equality(&self, condition: ExprId) -> Option<Ty> {
        self.when_subject_plans
            .nullable_ieee754_equalities
            .get(&condition)
            .copied()
    }

    /// The narrowed view of the subject an `is`/`in` condition reads, when earlier conditions
    /// narrowed the subject below its own type.
    pub(crate) fn when_subject_condition_view(&self, condition: ExprId) -> Option<Ty> {
        self.when_subject_plans
            .condition_views
            .get(&condition)
            .copied()
    }
}
