//! `if` expression checking and continuation-flow publication.

use crate::ast::{BinOp, Expr, ExprId};
use crate::types::Ty;

use super::receiver_flow::CompletedFlow;
use super::scope::ScopeKind;
use super::{conditional_branch, control_flow_join, Checker, CheckerScope, Wanted};

impl Checker<'_> {
    /// Check an `if` branch with its condition narrowings.
    ///
    /// The returned read types line up with [`Self::stable_local_vars`] of the continuation: the
    /// type each stable `var` has at the end of this edge.
    fn if_branch_ty(
        &mut self,
        scope: &CheckerScope<'_>,
        cond: ExprId,
        branch: ExprId,
        then: bool,
        wanted: Wanted,
    ) -> (Ty, Vec<Ty>, CompletedFlow) {
        let locals = self.stable_local_vars(scope);
        let declared = locals
            .iter()
            .map(|local| local.declared)
            .collect::<Vec<_>>();
        let entry_reads = self.local_edge_reads(scope, &locals, &declared);
        let (casts, declined) = self.condition_narrowings(scope, cond, then);
        let compound = matches!(self.file.expr(cond), Expr::Binary { op, .. } if *op
            == if then { BinOp::And } else { BinOp::Or });
        let branch_scope = scope.child(ScopeKind::Block);
        let scope = &branch_scope;
        self.apply_narrowings(scope, &casts, &declined, compound);
        self.apply_condition_exclusions(scope, cond, then);
        // `if (this is B)` narrows the implicit receiver to `B` for the branch body.
        let ty = self.with_this_narrow(self.this_is_narrowing(scope, cond, !then), |c| {
            let actual = c.expr_result(scope, branch, wanted.expected, wanted.value_required);
            let Some(expected) = wanted.expected else {
                return actual;
            };
            // A branch read may expose one projection of a proven intersection even though the
            // conditional's context asks for another. `v: T` in the false branch of `v !is A`
            // reads as `A`, but the same value is still `T`; project it while this branch's lexical
            // flow frame is alive so the enclosing join does not widen `T & A` to `Any?`.
            let projected =
                c.recorded_expression_type_for_expected(scope, branch, actual, expected);
            if projected != actual {
                // The numeric conversion already records the adaptation. The arithmetic
                // expression keeps its operand width, so `2147483647 + 1` wraps as `Int`
                // before that conversion becomes `Long`. Rewriting the expression type
                // would fold the same addition in 64 bits.
                let keeps_operand_width = c
                    .selected_numeric_conversions
                    .get(&branch)
                    .is_some_and(|target| *target == expected)
                    && c.integer_constant_provenance(branch).is_some();
                if keeps_operand_width {
                    projected
                } else {
                    c.set(branch, projected)
                }
            } else {
                actual
            }
        });
        let reads = self.local_edge_reads(scope, &locals, &entry_reads);
        let flow = CompletedFlow::capture(self, scope);
        (ty, reads, flow)
    }

    pub(super) fn expr_inner_if(
        &mut self,
        scope: &CheckerScope<'_>,
        e: ExprId,
        wanted: Wanted,
        cond: ExprId,
        then_branch: ExprId,
        else_branch: Option<ExprId>,
    ) -> Ty {
        let t = {
            let ct = self.expr(scope, cond);
            self.expect_assignable(Ty::Boolean, ct, self.span(cond), "if condition");
            let entry = scope.flow_snapshot();
            let locals = self.stable_local_vars(scope);
            let declared = locals
                .iter()
                .map(|local| local.declared)
                .collect::<Vec<_>>();
            let entry_reads = self.local_edge_reads(scope, &locals, &declared);
            let (tt, then_reads, mut then_flow) =
                self.if_branch_ty(scope, cond, then_branch, true, wanted);
            let mut then_exit = self.take_local_flow_exit(
                scope,
                &entry,
                then_reads,
                self.normal_completion(then_branch),
            );
            match else_branch {
                Some(eb) => {
                    let (et, else_reads, mut else_flow) =
                        self.if_branch_ty(scope, cond, eb, false, wanted);
                    let mut else_exit = self.take_local_flow_exit(
                        scope,
                        &entry,
                        else_reads,
                        self.normal_completion(eb),
                    );
                    let fixed = self.expectation_fixes_branches(scope, e, wanted.expected);
                    let tt =
                        self.rebind_conditional_branch(then_branch, et, tt, fixed, |c, exp| {
                            let (ty, reads, flow) = c.if_branch_ty(
                                scope,
                                cond,
                                then_branch,
                                true,
                                Wanted {
                                    expected: Some(exp),
                                    value_required: wanted.value_required,
                                },
                            );
                            then_exit = c.take_local_flow_exit(
                                scope,
                                &entry,
                                reads,
                                c.normal_completion(then_branch),
                            );
                            then_flow = flow;
                            ty
                        });
                    let et = self.rebind_conditional_branch(eb, tt, et, fixed, |c, exp| {
                        let (ty, reads, flow) = c.if_branch_ty(
                            scope,
                            cond,
                            eb,
                            false,
                            Wanted {
                                expected: Some(exp),
                                value_required: wanted.value_required,
                            },
                        );
                        else_exit =
                            c.take_local_flow_exit(scope, &entry, reads, c.normal_completion(eb));
                        else_flow = flow;
                        ty
                    });
                    self.report_unbound_conditional_branch(scope, then_branch);
                    self.report_unbound_conditional_branch(scope, eb);
                    self.publish_joined_local_flow(
                        scope,
                        &locals,
                        &entry_reads,
                        &[then_exit, else_exit],
                    );
                    let mut flows = Vec::new();
                    if self.completed_normally(then_branch) {
                        flows.push(then_flow);
                    }
                    if self.completed_normally(eb) {
                        flows.push(else_flow);
                    }
                    CompletedFlow::publish_common(self, scope, &flows);
                    conditional_branch::join_adapted_branches(
                        self,
                        scope,
                        wanted.expected,
                        then_branch,
                        tt,
                        eb,
                        et,
                        e,
                    )
                }
                None => {
                    self.report_unbound_conditional_branch(scope, then_branch);
                    let else_exit = control_flow_join::LocalFlowExit {
                        completes: true,
                        parent_flow: entry.clone(),
                        reads: self.implicit_false_edge_reads(scope, cond, &locals, &entry_reads),
                    };
                    self.publish_joined_local_flow(
                        scope,
                        &locals,
                        &entry_reads,
                        &[then_exit, else_exit],
                    );
                    Ty::Unit
                }
            }
        };
        self.set(e, t)
    }
}
