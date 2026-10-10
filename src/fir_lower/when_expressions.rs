//! Lower checked `when` decisions into common IR.
//!
//! The checker publishes one FIR identity for a subject and every predicate use. This boundary
//! materializes the subject once, as kotlinc's `tmp_subject`, and redirects those uses to it.
//!
//! Lowering owns only that semantic shape: it records the temporary and every use reads it.
//! Removing the physical store/load pair when the subject is read once belongs to the JVM
//! backend's bytecode temporaries pass, never to this module.

use crate::fir::{
    FirExprId, FirWhenBranch, FirWhenCondition, FirWhenSubjectNumericEquality, ResolvedTy,
};
use crate::ir::{ExprId, IrBinOp, IrExpr, IrTypeOp};
use crate::types::Ty;

use super::{BodyLowering, FirLoweringFailure, LoweringState};

impl BodyLowering<'_> {
    pub(super) fn when_expression(
        &mut self,
        subject: Option<FirExprId>,
        branches: &[FirWhenBranch],
        result_ty: Ty,
        deeply_exhaustive: bool,
        line: u32,
    ) -> Result<ExprId, FirLoweringFailure> {
        let mut prefix = Vec::new();
        let subject_expression = subject;
        let subject = subject
            .map(|subject| {
                let subject_expression = self
                    .body
                    .expr(subject)
                    .ok_or(FirLoweringFailure::MissingExpression(subject))?;
                let subject_ty = subject_expression.ty.get();
                let value = self.expression(subject)?;

                // Every subject gets its own value, a read of an immutable local included: kotlinc's
                // FIR-to-IR declares `tmp_subject` for any subject expression, and
                // `JvmOptimizationLowering` deliberately keeps one initialized from a variable read
                // (`dontTouchTemporaryVals`). Whether its store survives is then the bytecode
                // temporaries pass's decision, exactly as in kotlinc.
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
                    FirWhenCondition::SubjectEquals { candidate, numeric } => {
                        // fir2ir builds the subject comparison, and the subject read in it, at
                        // the condition's own offsets.
                        let line = self.body.expression_debug_lines(candidate).source;
                        let candidate = self.expression(candidate)?;
                        let subject = subject.ok_or(FirLoweringFailure::MissingWhenSubject {
                            origin: branch.origin,
                        })?;
                        let subject = self.ir.add_expr(IrExpr::GetValue(subject));
                        let (subject, candidate) =
                            self.when_subject_equality_operands(subject, candidate, numeric);
                        let comparison = self.ir.add_expr(IrExpr::PrimitiveBinOp {
                            op: IrBinOp::Eq,
                            lhs: subject,
                            rhs: candidate,
                        });
                        if line != 0 {
                            self.ir.expr_source_lines.insert(subject, line);
                            self.ir.expr_source_lines.insert(comparison, line);
                        }
                        comparison
                    }
                    FirWhenCondition::Predicate(candidate) => {
                        // fir2ir reads the subject afresh in each condition (`is T`, `in r`), at
                        // that condition's offsets, so the read carries the condition's line.
                        if let (Some(expression), Some(temporary)) = (subject_expression, subject) {
                            let read = self.ir.add_expr(IrExpr::GetValue(temporary));
                            let line = self.body.expression_debug_lines(candidate).source;
                            if line != 0 {
                                self.ir.expr_source_lines.insert(read, line);
                            }
                            self.set_expression_state(expression, LoweringState::Lowered(read));
                        }
                        self.expression(candidate)?
                    }
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
        // fir2ir gives a `when` without branches no `IrWhen` at all, only the block holding its
        // subject, so it has no `when` line of its own.
        if line != 0 && !branches.is_empty() {
            self.ir.whens.source_lines.insert(when, line);
        }
        let has_else = branches.iter().any(|branch| branch.conditions.is_empty());
        // Every `when` with an `else`, and one the checker proved exhaustive without it (whatever
        // its result, `Unit` included), is exhaustive; the checker also decided whether fir2ir types
        // it by its checked result.
        if has_else || deeply_exhaustive {
            let result_ty = if deeply_exhaustive {
                result_ty
            } else {
                Ty::Unit
            };
            self.ir.whens.exhaustive.insert(when, result_ty);
        }
        if prefix.is_empty() {
            Ok(when)
        } else {
            // The block holding the subject is the checked expression; the `when` inside it yields
            // the same value.
            self.ir.logical_types.insert(when, result_ty);
            Ok(self.ir.add_expr(IrExpr::Block {
                stmts: prefix,
                value: Some(when),
            }))
        }
    }

    /// Materialize the resolver's exact numeric equality plan. The declared subject stays a
    /// reference, so unboxing remains distinct from a later widening (`Float` to `Double`).
    fn when_subject_equality_operands(
        &mut self,
        subject: ExprId,
        candidate: ExprId,
        numeric: Option<FirWhenSubjectNumericEquality>,
    ) -> (ExprId, ExprId) {
        let Some(numeric) = numeric else {
            return (subject, candidate);
        };
        let subject = self.coerce_when_operand(subject, numeric.subject_unbox);
        let subject = numeric
            .subject_widening
            .map(|target| self.coerce_when_operand(subject, target))
            .unwrap_or(subject);
        let candidate = numeric
            .candidate_widening
            .map(|target| self.coerce_when_operand(candidate, target))
            .unwrap_or(candidate);
        (subject, candidate)
    }

    fn coerce_when_operand(&mut self, value: ExprId, target: ResolvedTy) -> ExprId {
        self.ir.add_expr(IrExpr::TypeOp {
            op: IrTypeOp::ImplicitCoercion,
            arg: value,
            type_operand: target.get(),
        })
    }
}
