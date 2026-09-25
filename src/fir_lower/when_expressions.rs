//! Lower checked `when` decisions into common IR.
//!
//! The checker publishes one FIR identity for a subject and every predicate use. This boundary
//! materializes an unstable subject once and redirects those uses to its snapshot.

use crate::fir::{FirExprId, FirWhenBranch, FirWhenCondition};
use crate::ir::{ExprId, IrBinOp, IrExpr};
use crate::types::Ty;

use super::{BodyLowering, FirLoweringFailure, LoweringState};

impl BodyLowering<'_> {
    pub(super) fn when_expression(
        &mut self,
        subject: Option<FirExprId>,
        branches: &[FirWhenBranch],
        result_ty: Ty,
    ) -> Result<ExprId, FirLoweringFailure> {
        let mut prefix = Vec::new();
        let subject = subject
            .map(|subject| {
                let subject_expression = self
                    .body
                    .expr(subject)
                    .ok_or(FirLoweringFailure::MissingExpression(subject))?;
                let subject_ty = subject_expression.ty.get();
                let value = self.expression(subject)?;

                // An immutable local is already a stable snapshot. Any other subject, including a
                // mutable local, needs a dedicated value because conditions may have side effects.
                if let Some(slot) = self.stable_value_read(subject, value) {
                    return Ok::<_, FirLoweringFailure>(slot);
                }

                let temporary = self.allocate_temporary();
                prefix.push(self.ir.add_expr(IrExpr::Variable {
                    index: temporary,
                    ty: subject_ty,
                    init: Some(value),
                    named: false,
                }));
                let read = self.ir.add_expr(IrExpr::GetValue(temporary));
                self.set_expression_state(subject, LoweringState::Lowered(read));
                Ok::<_, FirLoweringFailure>(temporary)
            })
            .transpose()?;

        let mut lowered_branches = Vec::with_capacity(branches.len());
        for branch in branches {
            let mut condition = None;
            for candidate in branch.conditions.iter().copied() {
                let candidate = match candidate {
                    FirWhenCondition::SubjectEquals(candidate) => {
                        let candidate = self.expression(candidate)?;
                        let subject = subject.ok_or(FirLoweringFailure::MissingWhenSubject {
                            origin: branch.origin,
                        })?;
                        let subject = self.ir.add_expr(IrExpr::GetValue(subject));
                        self.ir.add_expr(IrExpr::PrimitiveBinOp {
                            op: IrBinOp::Eq,
                            lhs: subject,
                            rhs: candidate,
                        })
                    }
                    FirWhenCondition::Predicate(candidate) => self.expression(candidate)?,
                };
                condition = Some(match condition {
                    Some(previous) => self.short_circuit_or(previous, candidate),
                    None => candidate,
                });
            }
            if let Some(guard) = branch.guard {
                let guard = self.expression(guard)?;
                condition = Some(match condition {
                    Some(previous) => self.short_circuit_and(previous, guard),
                    None => guard,
                });
            }
            lowered_branches.push((condition, self.expression(branch.result)?));
        }

        let when = self.ir.add_expr(IrExpr::When {
            branches: lowered_branches,
        });
        let has_else = branches.iter().any(|branch| branch.conditions.is_empty());
        // Keep the checker's result for every exhaustive `when`. A no-else Unit-valued expression
        // remains the one genuinely non-exhaustive statement form.
        if has_else || result_ty != Ty::Unit {
            self.ir.exhaustive_whens.insert(when, result_ty);
        }
        if prefix.is_empty() {
            Ok(when)
        } else {
            Ok(self.ir.add_expr(IrExpr::Block {
                stmts: prefix,
                value: Some(when),
            }))
        }
    }
}
