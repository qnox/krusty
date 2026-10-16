//! The read type of a stable local after a conditional.
//!
//! An `if` or `when` continues only along edges that finish normally. Each of those edges has its
//! own read type for a `var`: the condition facts that hold on that edge, then the assignments the
//! edge performs. The continuation reads the common supertype of those edge types, as kotlinc's
//! `LogicSystem.or` unifies the edges' type statements: `if (x == null) x = ArrayList()` leaves a
//! `Collection<String>?` local read as `Collection<String>`. A `return`, `break`, `continue`, or
//! `Nothing` edge does not finish, so it is not part of the join. A join that is the declared
//! type or `Any?` proves nothing and leaves the declared type. A unanimous `Nothing?` is an exact
//! flow fact: every completing edge stored `null`.

use crate::fx_hash::HashSet;

use crate::ast::{BinOp, Expr, ExprId};
use crate::types::Ty;

use super::scope::{FlowSnapshot, NarrowPath, Ns, PathRoot, ScopeKind};
use super::{Checker, CheckerScope, ReceiverFnValueOrigin};

/// One stable local `var` visible at the conditional, in nearest-binding order.
pub(super) struct StableLocal {
    name: String,
    flow_identity: u32,
    /// Type a write must satisfy. The continuation falls back to this when edges disagree.
    pub(super) declared: Ty,
}

/// Flow state of one conditional edge after its body has been checked.
pub(super) struct LocalFlowExit {
    pub(super) completes: bool,
    pub(super) parent_flow: FlowSnapshot,
    /// Read type of each [`StableLocal`], same order.
    pub(super) reads: Vec<Ty>,
    /// Every incomparable classifier facet proved for each [`StableLocal`], same order.
    pub(super) intersections: Vec<Vec<Ty>>,
}

impl Checker<'_> {
    /// Stable local `var`s visible here, nearest binding of each name only.
    pub(super) fn stable_local_vars(&self, scope: &CheckerScope<'_>) -> Vec<StableLocal> {
        let mut seen = HashSet::default();
        let mut locals = Vec::new();
        scope.visit_bindings(Ns::Value, |name, binding| {
            if !seen.insert(name.to_string()) {
                return;
            }
            let Some(local) = binding.value() else {
                return;
            };
            if !local.is_var || !matches!(local.origin, ReceiverFnValueOrigin::Local) {
                return;
            }
            locals.push(StableLocal {
                name: name.to_string(),
                flow_identity: local.flow_identity,
                declared: local.write_ty.unwrap_or(local.declared_ty),
            });
        });
        locals
    }

    /// Read type of each stable local on this edge.
    ///
    /// `unchanged` is the type to keep when this edge declares a different binding of the same
    /// name: that declaration hides the outer `var`, so the edge does not modify it.
    pub(super) fn local_edge_reads(
        &self,
        scope: &CheckerScope<'_>,
        locals: &[StableLocal],
        unchanged: &[Ty],
    ) -> Vec<Ty> {
        locals
            .iter()
            .enumerate()
            .map(|(index, local)| match self.lookup(scope, &local.name) {
                Some(found) if found.flow_identity == local.flow_identity => {
                    self.local_narrowing(scope, &local.name).unwrap_or(found.ty)
                }
                _ => unchanged[index],
            })
            .collect()
    }

    /// Flow-intersection constituents of each stable local on this edge.
    pub(super) fn local_edge_intersections(
        &self,
        scope: &CheckerScope<'_>,
        locals: &[StableLocal],
    ) -> Vec<Vec<Ty>> {
        locals
            .iter()
            .map(|local| {
                let path = NarrowPath::root_only(PathRoot::Value(local.flow_identity));
                self.lookup_intersection_narrowing(scope, &path)
            })
            .collect()
    }

    /// Whether evaluating `branch` can finish and continue after the conditional.
    ///
    /// `Nothing` is the checker's own result for a throw, a `Nothing` call, or a block that
    /// always transfers. A branch typed that way is not a predecessor of the continuation.
    pub(super) fn normal_completion(&self, branch: ExprId) -> bool {
        self.expr_types.get(branch.0 as usize).copied() != Some(Ty::Nothing)
            && !self.expr_diverges(branch)
    }

    /// A block always runs, so its assignments belong to the enclosing edge.
    ///
    /// The block frame dies with the block. Copy the facts it proved about outer `var`s onto the
    /// parent frame first; facts about a `var` declared in the block stay there.
    pub(super) fn promote_block_flow(&self, block: &CheckerScope<'_>) {
        let Some(parent) = block.parent() else {
            return;
        };
        for (name, ty) in block.own_local_flow_facts() {
            if block.declared_here(&name, Ns::Value) || ty == Ty::Error || ty.mentions_pending() {
                continue;
            }
            parent.record_local_flow_fact(&name, ty);
        }
    }

    /// Snapshot the continuation after a branch, then put the entry facts back so the next edge
    /// is checked from the same point.
    pub(super) fn take_local_flow_exit(
        &mut self,
        scope: &CheckerScope<'_>,
        entry: &FlowSnapshot,
        reads: Vec<Ty>,
        intersections: Vec<Vec<Ty>>,
        completes: bool,
    ) -> LocalFlowExit {
        let parent_flow = scope.flow_snapshot();
        scope.restore_flow(entry);
        LocalFlowExit {
            completes,
            parent_flow,
            reads,
            intersections,
        }
    }

    /// Read types on the implicit else of `if (cond)`: the condition is false and nothing is
    /// assigned. The facts live on a temporary frame, so the continuation's own frame stays put.
    pub(super) fn implicit_false_edge_state(
        &mut self,
        scope: &CheckerScope<'_>,
        cond: ExprId,
        locals: &[StableLocal],
        entry_reads: &[Ty],
    ) -> (Vec<Ty>, Vec<Vec<Ty>>) {
        let (casts, declined) = self.condition_narrowings(scope, cond, false);
        let compound = matches!(self.file.expr(cond), Expr::Binary { op: BinOp::Or, .. });
        let branch_scope = scope.child(ScopeKind::Block);
        self.apply_narrowings(&branch_scope, &casts, &declined, compound);
        self.apply_condition_exclusions(&branch_scope, cond, false);
        (
            self.local_edge_reads(&branch_scope, locals, entry_reads),
            self.local_edge_intersections(&branch_scope, locals),
        )
    }

    /// Install the join of `exits` on the continuation.
    ///
    /// Parent-frame facts survive only when every completing edge still has them. A stricter type
    /// that the edges agree on, including one proved only inside a branch, is recorded on this
    /// frame afterwards.
    pub(super) fn publish_joined_local_flow(
        &mut self,
        scope: &CheckerScope<'_>,
        locals: &[StableLocal],
        entry_reads: &[Ty],
        exits: &[LocalFlowExit],
    ) {
        let completing = exits
            .iter()
            .filter(|exit| exit.completes)
            .collect::<Vec<_>>();
        if completing.is_empty() {
            return;
        }
        let parent_flows = completing
            .iter()
            .map(|exit| exit.parent_flow.clone())
            .collect::<Vec<_>>();
        scope.restore_common_flow(&parent_flows);
        for (index, local) in locals.iter().enumerate() {
            let reads = completing
                .iter()
                .map(|exit| exit.reads[index])
                .collect::<Vec<_>>();
            let edge_intersections = completing
                .iter()
                .map(|exit| exit.intersections[index].as_slice())
                .collect::<Vec<_>>();
            let (joined, intersection) = self.joined_edge_reads(&reads, &edge_intersections);
            // A declared upper bound remains a representable fallback when the exact common type
            // is an intersection that `Ty` cannot publish. This is the same fixed-expectation rule
            // conditional-result joining uses.
            let declared_non_null = crate::resolve::definitely_non_null_ty(local.declared);
            let joined = joined
                .filter(|&ty| self.receiver_is_assignable(ty, local.declared))
                .or_else(|| {
                    (declared_non_null != Ty::Error
                        && !declared_non_null.mentions_pending()
                        && reads.iter().all(|&read| {
                            read != Ty::Error
                                && !read.mentions_pending()
                                && self.receiver_is_assignable(read, declared_non_null)
                        }))
                    .then_some(declared_non_null)
                });
            let entry_ty = entry_reads[index];
            let install = joined.filter(|&ty| {
                ty != local.declared && ty != Ty::nullable(Ty::obj_name(crate::types::wk::any()))
            });
            if let Some(ty) = install {
                crate::trace_compiler!(
                    "smartcast",
                    "join local {} as {ty:?} (entry {entry_ty:?})",
                    local.name,
                );
                scope.record_local_flow_fact(&local.name, ty);
            } else if entry_ty != local.declared {
                crate::trace_compiler!(
                    "smartcast",
                    "join local {} dropped entry {entry_ty:?}; edges disagree",
                    local.name,
                );
                scope.narrow_local(&local.name, None);
            }
            // A denotable primary view can equal the declaration while another incomparable
            // common facet is still new information. Publish the complete intersection even when
            // no narrower single read type was installed.
            if intersection.len() > 1 {
                let path = NarrowPath::root_only(PathRoot::Value(local.flow_identity));
                for constituent in intersection {
                    scope.narrow_intersection(path.clone(), constituent);
                }
            }
        }
    }

    /// The common read type and every incomparable common classifier facet of completing edges.
    ///
    /// A pairwise fold is lossy: `First` and `Second` may initially share both `Source<String>` and
    /// `Tagged`, which has no single denotable type, while a later `Third` removes `Tagged` and
    /// leaves the denotable `Source<CharSequence>`. Intersect all applied hierarchies together
    /// before choosing one read projection, and retain multiple final facets as lexical flow facts.
    fn joined_edge_reads(&self, reads: &[Ty], intersections: &[&[Ty]]) -> (Option<Ty>, Vec<Ty>) {
        if reads
            .iter()
            .any(|ty| *ty == Ty::Error || ty.mentions_pending())
        {
            return (None, Vec::new());
        }
        let edge_views = reads
            .iter()
            .copied()
            .zip(intersections)
            .map(|(read, constituents)| {
                let mut views = constituents.to_vec();
                if !views.contains(&read) {
                    views.push(read);
                }
                views
            })
            .collect::<Vec<_>>();
        let intersection = self.minimal_common_classifier_views_for_edges(&edge_views);
        let joined = intersection.first().copied().or_else(|| {
            let (&first, rest) = reads.split_first()?;
            rest.iter().try_fold(first, |joined, &read| {
                self.semantic_common_supertype(joined, read)
            })
        });
        (joined, intersection)
    }
}
