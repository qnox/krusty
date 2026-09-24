//! Checked-FIR publication for `when` expressions.
//!
//! Subject predicates retain the parsed subject expression as an operand. This boundary replaces
//! that syntax node with the subject's single checked value while the branches are published.

use crate::ast::{ExprId, WhenArm};
use crate::fir::{FirWhenBranch, FirWhenCondition};

use super::{BodyCheckFailure, BodyFirChecker, FirExprKind};

impl BodyFirChecker<'_> {
    pub(super) fn when_expression(
        &mut self,
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

        Ok(FirExprKind::When {
            subject,
            branches: branches?.into_boxed_slice(),
        })
    }
}
