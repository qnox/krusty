//! `try` expression checking: the body and catches as branches of one conditional.
//!
//! kotlinc completes a `try` with catches as a synthetic call over its body and catch results, as
//! it does an `if` or a `when`. A branch whose generic call leaves a result formal open
//! (`try { parse() } catch (e: Exception) { emptyList() }`) is rebound against its siblings before
//! the branches join.

use crate::ast::{CatchClause, ExprId};
use crate::types::Ty;

use super::catch_flow::TryBodyWrites;
use super::control_flow_join::{LocalFlowExit, StableLocal};
use super::scope::{FlowSnapshot, ScopeKind};
use super::{conditional_branch, Checker, CheckerScope, Wanted};

/// One checked catch clause: its body, the caught type, and what its check left behind.
struct CheckedCatch {
    body: ExprId,
    caught: Ty,
    ty: Ty,
    writes: TryBodyWrites,
    exit: LocalFlowExit,
}

/// A catch clause with the stable locals whose read types its exit joins.
struct CatchEdge<'a> {
    clause: &'a CatchClause,
    locals: &'a [StableLocal],
    entry_reads: &'a [Ty],
}

impl Checker<'_> {
    pub(super) fn expr_inner_try(
        &mut self,
        scope: &CheckerScope<'_>,
        e: ExprId,
        wanted: Wanted,
        body: ExprId,
        catches: Vec<CatchClause>,
        finally: Option<ExprId>,
    ) -> Ty {
        let joined = !catches.is_empty();
        // A catch begins on an exceptional edge from any point of the try body: it gets the
        // entry facts minus every narrowing the body writes. Normal exits keep only the facts
        // common to all of them. No syntax coordinate or body survives FIR.
        let entry_flow = scope.flow_snapshot();
        let locals = self.stable_local_vars(scope);
        let declared = locals
            .iter()
            .map(|local| local.declared)
            .collect::<Vec<_>>();
        let entry_reads = self.local_edge_reads(scope, &locals, &declared);
        let (mut body_ty, mut written) = self.try_body_written_bindings(|checker| {
            checker.joined_branch_result(scope, body, wanted, joined)
        });
        let mut body_exit = self.try_edge_exit(scope, &locals, &entry_reads, body);
        let mut checked_catches = Vec::with_capacity(catches.len());
        for c in &catches {
            let caught = self.caught_type(scope, c);
            let (ty, writes, exit) = self.check_catch(
                scope,
                &entry_flow,
                &written,
                CatchEdge {
                    clause: c,
                    locals: &locals,
                    entry_reads: &entry_reads,
                },
                caught,
                |checker, scope| checker.joined_branch_result(scope, c.body, wanted, joined),
            );
            checked_catches.push(CheckedCatch {
                body: c.body,
                caught,
                ty,
                writes,
                exit,
            });
        }

        // Recheck a branch whose generic call left its result open against the other branches.
        let fixed = self.expectation_fixes_branches(scope, e, wanted.expected);
        if !checked_catches.is_empty() {
            for index in 0..=checked_catches.len() {
                let branches = std::iter::once((body, body_ty))
                    .chain(checked_catches.iter().map(|c| (c.body, c.ty)))
                    .collect::<Vec<_>>();
                let Some(sibling) = self.conditional_sibling_evidence(&branches, index) else {
                    continue;
                };
                let (branch, current) = branches[index];
                if index == 0 {
                    let mut replaced = None;
                    body_ty = self.rebind_conditional_branch(
                        branch,
                        sibling,
                        current,
                        fixed,
                        |checker, expectation| {
                            scope.restore_flow(&entry_flow);
                            let (ty, writes) = checker.try_body_written_bindings(|checker| {
                                checker.expr_result(
                                    scope,
                                    body,
                                    Some(expectation),
                                    wanted.value_required,
                                )
                            });
                            let exit = checker.try_edge_exit(scope, &locals, &entry_reads, body);
                            replaced = Some((writes, exit));
                            ty
                        },
                    );
                    if let Some((writes, exit)) = replaced {
                        written = writes;
                        body_exit = exit;
                    }
                } else {
                    let catch = &catches[index - 1];
                    let caught = checked_catches[index - 1].caught;
                    let mut replaced = None;
                    let ty = self.rebind_conditional_branch(
                        branch,
                        sibling,
                        current,
                        fixed,
                        |checker, expectation| {
                            let (ty, writes, exit) = checker.check_catch(
                                scope,
                                &entry_flow,
                                &written,
                                CatchEdge {
                                    clause: catch,
                                    locals: &locals,
                                    entry_reads: &entry_reads,
                                },
                                caught,
                                |checker, scope| {
                                    checker.expr_result(
                                        scope,
                                        catch.body,
                                        Some(expectation),
                                        wanted.value_required,
                                    )
                                },
                            );
                            replaced = Some((writes, exit));
                            ty
                        },
                    );
                    let checked = &mut checked_catches[index - 1];
                    checked.ty = ty;
                    if let Some((writes, exit)) = replaced {
                        checked.writes = writes;
                        checked.exit = exit;
                    }
                }
            }
        }

        let mut result = body_ty;
        let mut branch_types = vec![(body, body_ty)];
        let mut exits = vec![body_exit];
        let mut all_exit_writes = TryBodyWrites::default();
        for checked in checked_catches {
            let ht = checked.ty;
            all_exit_writes.extend(checked.writes);
            exits.push(checked.exit);
            branch_types.push((checked.body, ht));
            // In VALUE position, REFERENCE branches use the same full join as other conditional
            // expressions: `try { x } catch { null }` is `T?`, and different reference classes
            // join to `Any`. Two primitive branches deliberately do not take it: an integer
            // constant is adapted to a sibling primitive after this loop, and a non-constant
            // primitive disagreement (`Int` versus `Long`) keeps `try_branch_join`.
            let reference_like =
                |ty: Ty| ty.is_reference() || matches!(ty, Ty::Nothing | Ty::Error);
            // A primitive beside a REAL reference joins the same way: `try { s.length } catch
            // (e: Exception) { null }` is `Int?`, as `if (c) s.length else null` is.
            let joins = (reference_like(result) && reference_like(ht))
                || result.is_reference()
                || ht.is_reference();
            result = if wanted.value_required && joins {
                // Join against the EXPECTATION, exactly as `if`/`when` do: a `try` in value
                // position is a conditional expression like any other. A blind join of two
                // generic branches invents an out-projection (`R<out Any>`) that no INVARIANT
                // declared type can take, rejecting source kotlinc accepts.
                conditional_branch::join_types(self, scope, wanted.expected, result, ht, e)
            } else {
                conditional_branch::try_branch_join(self, wanted.value_required, result, ht)
            };
        }
        result = conditional_branch::adapted_try_result(
            self,
            wanted.value_required,
            &branch_types,
            result,
        );
        self.complete_conditional_branches(
            scope,
            e,
            wanted.expected,
            result,
            branch_types.iter().copied(),
        );
        self.record_flexible_join(
            e,
            branch_types.iter().map(|(expression, _)| *expression),
            result,
            false,
        );
        self.publish_conditional_intersection(e, branch_types.iter().copied(), result);
        self.publish_joined_local_flow(scope, &locals, &entry_reads, &exits);
        let normal_continuation = scope.flow_snapshot();
        all_exit_writes.extend(written);
        if let Some(f) = finally {
            // A finalizer runs after every try/catch exit, including an abrupt return, throw,
            // break, or continue. Start it from the try entry with every binding written on
            // any reachable exit cleared; branch-only facts from a normal exit are not valid
            // on an abrupt one. Then project the finalizer's single checked flow delta onto
            // the normal-only continuation instead of checking its body a second time.
            scope.restore_flow(&entry_flow);
            self.clear_narrowings_a_try_body_writes(scope, &all_exit_writes);
            let finalizer_entry = scope.flow_snapshot();
            let (_, finalizer_writes) =
                self.try_body_written_bindings(|checker| checker.expr_statement(scope, f));
            let finalizer_local_flow = self.written_local_flow(scope, &finalizer_writes);
            let finalizer_exit = scope.flow_snapshot();
            scope.restore_flow_with_delta(&normal_continuation, &finalizer_entry, &finalizer_exit);
            // The delta carries every flow facet established by the finalizer: local read
            // types, path/intersection facts, exclusions, and saved Boolean implications.
            // Reapply exact local write results as well: a finalizer assignment dominates the
            // normal continuation even when it left the same fact as the conservative entry.
            for (name, narrowing) in finalizer_local_flow {
                self.record_local_read_fact(scope, &name, narrowing);
            }
        }
        self.set(e, result)
    }

    /// The type a catch clause binds, reporting a parameter that is not a non-null `Throwable`.
    fn caught_type(&mut self, scope: &CheckerScope<'_>, c: &CatchClause) -> Ty {
        let resolved_catch = self.type_ref_ty(scope, &c.ty);
        let catch_is_throwable = std::iter::once(resolved_catch)
            .chain(resolved_catch.ty_param_bound())
            .chain(self.semantic_tparam_extra_bounds(scope, resolved_catch))
            .any(|candidate| {
                crate::assignable::is_subtype(
                    &crate::assignable::TyCtx::new(),
                    self,
                    candidate,
                    Ty::obj("kotlin/Throwable"),
                )
            });
        crate::trace_compiler!(
            "resolve",
            "catch type resolved={resolved_catch:?} primary={:?} extra={:?} throwable={catch_is_throwable}",
            resolved_catch.ty_param_bound(),
            self.semantic_tparam_extra_bounds(scope, resolved_catch),
        );
        if resolved_catch.contains_error() {
            self.report_unresolved_type_ref(&c.ty);
            Ty::Error
        } else if !c.ty.nullable() && catch_is_throwable {
            resolved_catch
        } else {
            self.diags.error(
                c.param_span,
                format!(
                    "throwable type mismatch: actual type is '{}'.",
                    resolved_catch.source_name()
                ),
            );
            Ty::Error
        }
    }

    /// Check one catch body on the exceptional edge from the try entry: the entry facts minus the
    /// narrowings of every binding the try body writes, with the caught parameter in scope.
    fn check_catch(
        &mut self,
        scope: &CheckerScope<'_>,
        entry_flow: &FlowSnapshot,
        body_writes: &TryBodyWrites,
        edge: CatchEdge<'_>,
        caught: Ty,
        check_body: impl FnOnce(&mut Self, &CheckerScope<'_>) -> Ty,
    ) -> (Ty, TryBodyWrites, LocalFlowExit) {
        let c = edge.clause;
        scope.restore_flow(entry_flow);
        self.clear_narrowings_a_try_body_writes(scope, body_writes);
        let (ty, writes) = self.try_body_written_bindings(|checker| {
            let catch_scope = scope.child(ScopeKind::Block);
            checker.declare(&catch_scope, &c.name, caught, false);
            checker.record_lexical_declaration(
                &catch_scope,
                &c.name,
                super::lexical_bindings::LexicalValueRole::Catch(c.param_span),
            );
            let ty = check_body(checker, &catch_scope);
            // The catch frame dies here, but the clause always runs on its edge: its
            // assignments to outer locals belong to the exit the continuation joins.
            checker.promote_block_flow(&catch_scope);
            ty
        });
        let exit = self.try_edge_exit(scope, edge.locals, edge.entry_reads, c.body);
        (ty, writes, exit)
    }

    /// The state one try or catch edge leaves for the continuation: the flow it ends in and the
    /// read type of every stable local, joined with its siblings by `publish_joined_local_flow`.
    fn try_edge_exit(
        &self,
        scope: &CheckerScope<'_>,
        locals: &[StableLocal],
        entry_reads: &[Ty],
        body: ExprId,
    ) -> LocalFlowExit {
        LocalFlowExit {
            completes: self.normal_completion(body),
            parent_flow: scope.flow_snapshot(),
            reads: self.local_edge_reads(scope, locals, entry_reads),
            intersections: self.local_edge_intersections(scope, locals),
        }
    }
}
