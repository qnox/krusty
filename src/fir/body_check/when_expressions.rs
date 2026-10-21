//! Checked-FIR publication for `when` and `if` expressions.
//!
//! Subject predicates retain the parsed subject expression as an operand. This boundary replaces
//! that syntax node with the subject's single checked value while the branches are published.

use crate::ast::{ExprId, WhenArm};
use crate::fir::{
    FirExpr, FirExprId, FirWhenBranch, FirWhenCondition, FirWhenSubjectNumericEquality, ResolvedTy,
    SyntheticOriginKind,
};
use crate::types::{EqualityMode, Ty};

use super::{BodyCheckFailure, BodyFirChecker, FirExprKind};

impl BodyFirChecker<'_> {
    /// An `if`: each branch converted to the expression's type, and a missing `else` published as
    /// an empty `Unit` block, which leaves the conditional not exhaustive. The published
    /// conditional carries whether fir2ir types it by its result ([`Self::else_is_exhaustive`]).
    pub(super) fn conditional(
        &mut self,
        expression: ExprId,
        cond: ExprId,
        then_branch: ExprId,
        else_branch: Option<ExprId>,
    ) -> Result<FirExprKind, BodyCheckFailure> {
        let result_ty = self.expression_type(expression)?;
        let then_origin = self.expression_origin(then_branch)?;
        let checked_then = self.expression(then_branch)?;
        let then_conversion =
            self.selected_value_conversion(then_branch, checked_then, result_ty, then_origin)?;
        let written_else = else_branch.is_some();
        let (else_branch, else_conversion) = match else_branch {
            Some(else_branch) => {
                let else_origin = self.expression_origin(else_branch)?;
                let checked_else = self.expression(else_branch)?;
                (
                    checked_else,
                    self.selected_value_conversion(
                        else_branch,
                        checked_else,
                        result_ty,
                        else_origin,
                    )?,
                )
            }
            None => {
                let cause = self.expression_origin(expression)?;
                let origin = self
                    .origins
                    .synthetic(cause, SyntheticOriginKind::MissingElseUnit);
                (
                    self.body.add_expr(FirExpr {
                        origin,
                        ty: ResolvedTy::new(Ty::Unit).expect("Unit is a publishable FIR type"),
                        kind: FirExprKind::Block {
                            statements: Box::new([]),
                            result: None,
                        },
                    }),
                    None,
                )
            }
        };
        Ok(FirExprKind::Conditional {
            condition: self.boolean_condition(cond)?,
            then_branch: checked_then,
            then_conversion,
            else_branch,
            else_conversion,
            deeply_exhaustive: written_else && self.else_is_exhaustive(else_branch),
        })
    }

    pub(super) fn when_expression(
        &mut self,
        expression: ExprId,
        subject_syntax: Option<ExprId>,
        arms: &[WhenArm],
    ) -> Result<FirExprKind, BodyCheckFailure> {
        let has_subject = subject_syntax.is_some();
        let subject = subject_syntax
            .map(|subject| self.expression(subject))
            .transpose()?;
        let subject_variable = if self.file.when_subject_variables.contains(&expression.0) {
            let variable = subject
                .and_then(|subject| self.body.expr(subject))
                .and_then(|subject| match &subject.kind {
                    FirExprKind::ValueRead(variable) => Some(*variable),
                    _ => None,
                })
                .ok_or_else(|| {
                    self.failure(
                        self.file.expr_span(expression),
                        super::BodyCheckFailureKind::MalformedWhenSubjectVariable,
                    )
                })?;
            Some(variable)
        } else {
            None
        };

        // `is`/`!is` and `in`/`!in` conditions contain the subject syntax as an operand. Check
        // every one against the already-published subject rather than publishing another copy.
        let substituted = match (subject_syntax, subject) {
            (Some(syntax), Some(checked))
                if !self.expression_substitutions.contains_key(&syntax) =>
            {
                self.expression_substitutions.insert(syntax, checked);
                Some(syntax)
            }
            _ => None,
        };

        let branches = arms
            .iter()
            .map(|arm| {
                let origin = self.expression_origin(arm.body)?;
                let conditions = arm
                    .conditions
                    .iter()
                    .map(|condition| {
                        let checked = if has_subject && !condition.is_predicate() {
                            self.expression(condition.expression())?
                        } else if let (Some(syntax), Some(subject)) = (substituted, subject) {
                            self.subject_predicate(condition.expression(), syntax, subject)?
                        } else {
                            self.boolean_condition(condition.expression())?
                        };
                        Ok(if has_subject && !condition.is_predicate() {
                            let plan = self
                                .info
                                .when_subject_numeric_equality(condition.expression());
                            let nullable_ieee754 = self
                                .info
                                .when_subject_nullable_ieee754_equality(condition.expression());
                            let resolved = |ty: Ty| {
                                let span = self.file.expr_span(condition.expression()).ok_or_else(
                                    || {
                                        self.failure(
                                            None,
                                            super::BodyCheckFailureKind::MissingSourceSpan,
                                        )
                                    },
                                )?;
                                self.resolved_type(span, ty)
                            };
                            let numeric = plan
                                .map(|plan| {
                                    Ok::<_, BodyCheckFailure>(FirWhenSubjectNumericEquality {
                                        subject_unbox: resolved(plan.subject_unbox())?,
                                        subject_widening: plan
                                            .subject_widening()
                                            .map(resolved)
                                            .transpose()?,
                                        candidate_widening: plan
                                            .candidate_widening()
                                            .map(resolved)
                                            .transpose()?,
                                    })
                                })
                                .transpose()?;
                            let nullable_ieee754 = nullable_ieee754.map(resolved).transpose()?;
                            let subject_ty = numeric
                                .map(|plan| {
                                    plan.subject_widening.unwrap_or(plan.subject_unbox).get()
                                })
                                .unwrap_or_else(|| {
                                    self.body
                                        .expr(subject.expect("subject equality has a subject"))
                                        .expect("the subject was just checked")
                                        .ty
                                        .get()
                                });
                            let candidate_ty = numeric
                                .and_then(|plan| plan.candidate_widening)
                                .unwrap_or_else(|| {
                                    self.body
                                        .expr(checked)
                                        .expect("the candidate was just checked")
                                        .ty
                                })
                                .get();
                            FirWhenCondition::SubjectEquals {
                                candidate: checked,
                                mode: EqualityMode::of_source_operands(subject_ty, candidate_ty),
                                numeric,
                                nullable_ieee754,
                            }
                        } else {
                            FirWhenCondition::Predicate(checked)
                        })
                    })
                    .collect::<Result<Vec<_>, BodyCheckFailure>>()?;
                let guard = arm
                    .guard
                    .map(|guard| self.boolean_condition(guard))
                    .transpose()?;
                let checked = self.expression(arm.body)?;
                // A branch written as a lambda, or as a block whose value is one, is still that
                // function until the conversion recorded for the expected fun interface is applied.
                let result = if self.branch_has_recorded_sam(arm.body) {
                    let target = self.expression_type(expression)?;
                    self.with_recorded_sam_conversion(arm.body, checked, target)?
                } else {
                    checked
                };
                Ok(FirWhenBranch {
                    origin,
                    conditions: conditions.into_boxed_slice(),
                    guard,
                    result,
                })
            })
            .collect::<Result<Vec<_>, BodyCheckFailure>>();

        // Do not leak a transient substitution when a condition or branch fails to publish.
        if let Some(syntax) = substituted {
            self.expression_substitutions.remove(&syntax);
        }

        let branches = branches?;
        // The resolver's verdict covers an `else` and a subject every branch covers; an `else`
        // that is an open `else if` chain still leaves the `when` short of deeply exhaustive.
        let deeply_exhaustive = self.info.when_is_exhaustive(expression)
            && match branches.last() {
                Some(last) if last.conditions.is_empty() => self.else_is_exhaustive(last.result),
                _ => true,
            };
        Ok(FirExprKind::When {
            subject,
            subject_variable,
            branches: branches.into_boxed_slice(),
            deeply_exhaustive,
        })
    }

    /// An `is`/`in` condition over the subject. It reads the subject as its arm sees it: when
    /// earlier conditions narrowed the subject (past a `null` arm, or a failed `!is`), that is a
    /// smart cast or unboxing of the subject's single value, kotlinc's smart-cast `$subj$`, and
    /// the subject itself keeps its own type.
    fn subject_predicate(
        &mut self,
        condition: ExprId,
        syntax: ExprId,
        subject: FirExprId,
    ) -> Result<FirExprId, BodyCheckFailure> {
        let Some(view) = self.info.when_subject_condition_view(condition) else {
            return self.boolean_condition(condition);
        };
        let span = self
            .file
            .expr_span(condition)
            .ok_or_else(|| self.failure(None, super::BodyCheckFailureKind::MissingSourceSpan))?;
        let view = self.resolved_type(span, view)?;
        let storage = self.expression_type(syntax)?;
        let origin = self.expression_origin(syntax)?;
        let read = self.stored_value_view(subject, storage, view, origin);
        self.expression_substitutions.insert(syntax, read);
        let checked = self.boolean_condition(condition);
        self.expression_substitutions.insert(syntax, subject);
        checked
    }

    /// Whether an `else` result keeps its `if`/`when` deeply exhaustive: an `else if` chain (a
    /// conditional published as the `else` result itself, not inside a block) must be deeply
    /// exhaustive in turn, which was decided when it was published.
    fn else_is_exhaustive(&self, else_result: crate::fir::FirExprId) -> bool {
        match self
            .body
            .expr(else_result)
            .map(|expression| &expression.kind)
        {
            Some(FirExprKind::Conditional {
                deeply_exhaustive, ..
            }) => *deeply_exhaustive,
            _ => true,
        }
    }
}
