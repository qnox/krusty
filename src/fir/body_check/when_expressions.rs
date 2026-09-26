//! Checked-FIR publication for `when` and `if` expressions.
//!
//! Subject predicates retain the parsed subject expression as an operand. This boundary replaces
//! that syntax node with the subject's single checked value while the branches are published.

use crate::ast::{ExprId, WhenArm};
use crate::fir::{FirExpr, FirWhenBranch, FirWhenCondition, ResolvedTy, SyntheticOriginKind};
use crate::types::Ty;

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
                        } else {
                            self.boolean_condition(condition.expression())?
                        };
                        Ok(if has_subject && !condition.is_predicate() {
                            FirWhenCondition::SubjectEquals(checked)
                        } else {
                            FirWhenCondition::Predicate(checked)
                        })
                    })
                    .collect::<Result<Vec<_>, BodyCheckFailure>>()?;
                let guard = arm
                    .guard
                    .map(|guard| self.boolean_condition(guard))
                    .transpose()?;
                Ok(FirWhenBranch {
                    origin,
                    conditions: conditions.into_boxed_slice(),
                    guard,
                    result: self.expression(arm.body)?,
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
            branches: branches.into_boxed_slice(),
            deeply_exhaustive,
        })
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
