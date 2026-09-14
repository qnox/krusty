//! Binding-owned implications carried by nullable safe-call results.
//!
//! If an immutable local `result` is initialized by `origin?.selector`, proving `result` non-null
//! also proves that exact `origin` binding non-null. The edge is stored on `result`'s lexical
//! binding by the checker; this module owns syntax extraction and cycle-safe graph traversal.

use std::collections::HashSet;

use crate::ast::{Expr, ExprId};
use crate::diag::Span;
use crate::types::Ty;

use super::scope::{NarrowPath, Ns};
use super::{Checker, CheckerScope, Local, ReceiverFnValueOrigin, ScopeBinding};

/// Ephemeral identity of one lexical value during a bounded checker run.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(super) struct BindingIdentity(u32);

impl Checker<'_> {
    pub(super) fn attach_safe_call_origin(
        &self,
        scope: &CheckerScope<'_>,
        name: &str,
        origin: BindingIdentity,
    ) {
        scope.rebind_nearest(name, Ns::Value, |binding, _| match binding {
            ScopeBinding::Value(mut local) => {
                local.safe_call_origin = Some(origin);
                ScopeBinding::Value(local)
            }
            other => other.clone(),
        });
    }

    /// Record only implications between immutable, direct lexical value slots. In particular, a
    /// local delegated `val` is a `getValue` call and can change between the safe call and proof.
    pub(super) fn safe_call_origin_for_initializer(
        &self,
        scope: &CheckerScope<'_>,
        expression: ExprId,
        result_is_var: bool,
    ) -> Option<BindingIdentity> {
        if result_is_var {
            return None;
        }
        let Expr::SafeCall { receiver, .. } = self.file.expr(expression) else {
            return None;
        };
        let mut base = *receiver;
        while let Expr::SafeCall { receiver, .. } = self.file.expr(base) {
            base = *receiver;
        }
        let Expr::Name(name) = self.file.expr(base) else {
            return None;
        };
        self.lookup(scope, name)
            .filter(|origin| {
                !origin.is_var
                    && origin.delegate_storage_ty.is_none()
                    && matches!(origin.origin, ReceiverFnValueOrigin::Local)
            })
            .and_then(|origin| origin.lexical_capture_identity)
            .map(BindingIdentity)
    }

    /// Find the currently visible spelling of one exact lexical binding. A same-named inner value
    /// makes the older binding inaccessible and therefore stops implication traversal.
    fn visible_local_binding(
        &self,
        scope: &CheckerScope<'_>,
        identity: BindingIdentity,
    ) -> Option<(String, Local)> {
        let mut found = None;
        scope.visit_bindings(Ns::Value, |name, binding| {
            if found.is_none() {
                if let Some(local) = binding
                    .value()
                    .filter(|local| local.lexical_capture_identity == Some(identity.0))
                {
                    found = Some((name.to_string(), local));
                }
            }
        });
        let (name, local) = found?;
        (self.lookup(scope, &name)?.lexical_capture_identity == Some(identity.0))
            .then_some((name, local))
    }

    /// Binding-identified safe-call origins reachable from one root-only lexical result.
    fn safe_call_origin_paths(
        &self,
        scope: &CheckerScope<'_>,
        path: &NarrowPath,
    ) -> Vec<NarrowPath> {
        if !path.segments.is_empty() {
            return Vec::new();
        }
        let Some(start) = self
            .lookup(scope, &path.root)
            .and_then(|local| local.lexical_capture_identity)
            .map(BindingIdentity)
        else {
            return Vec::new();
        };
        origin_chain(start, |identity| {
            self.visible_local_binding(scope, identity)?
                .1
                .safe_call_origin
        })
        .into_iter()
        .filter_map(|identity| {
            self.visible_local_binding(scope, identity)
                .map(|(name, _)| NarrowPath::root_only(&name))
        })
        .collect()
    }

    /// Narrow every binding-identified safe-call origin reachable from `path` on a branch that has
    /// already proven `path` non-null.
    pub(super) fn apply_safe_call_origin_narrowing(
        &mut self,
        scope: &CheckerScope<'_>,
        path: &NarrowPath,
        site: Span,
    ) {
        for origin in self.safe_call_origin_paths(scope, path) {
            if let Some(Ty::Nullable(inner)) = self.stable_path_ty(scope, &origin, site) {
                self.apply_narrowing_unchecked(scope, &origin, *inner);
            }
        }
    }

    pub(super) fn null_proof_or_decline(
        &self,
        scope: &CheckerScope<'_>,
        path: &NarrowPath,
        out: &mut Vec<(NarrowPath, Ty)>,
        declined: &mut Vec<(String, Ty)>,
        site: Span,
    ) {
        let origins = self.safe_call_origin_paths(scope, path);
        for path in std::iter::once(path.clone()).chain(origins) {
            match self.stable_path_ty(scope, &path, site) {
                Some(Ty::Nullable(inner)) => out.push((path, *inner)),
                _ => {
                    if let Some((name, Ty::Nullable(inner))) =
                        self.closure_mutated_decline(scope, &path, site)
                    {
                        declined.push((name, *inner));
                    }
                }
            }
        }
    }
}

impl Local {
    pub(super) const fn has_unstable_delegated_read(self) -> bool {
        self.delegate_storage_ty.is_some()
    }
}

/// Follow binding-to-binding implications once each. Source declaration order makes a correctly
/// identified graph acyclic, but explicit cycle detection keeps malformed/intermediate graphs
/// finite without imposing an arbitrary maximum on valid chains.
fn origin_chain(
    start: BindingIdentity,
    mut origin_of: impl FnMut(BindingIdentity) -> Option<BindingIdentity>,
) -> Vec<BindingIdentity> {
    let mut seen = HashSet::from([start]);
    let mut current = start;
    let mut origins = Vec::new();
    while let Some(origin) = origin_of(current) {
        if !seen.insert(origin) {
            break;
        }
        origins.push(origin);
        current = origin;
    }
    origins
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn traversal_has_no_valid_depth_cap() {
        let edges = (1..=32)
            .map(|identity| (BindingIdentity(identity), BindingIdentity(identity - 1)))
            .collect::<HashMap<_, _>>();
        let chain = origin_chain(BindingIdentity(32), |identity| {
            edges.get(&identity).copied()
        });
        assert_eq!(chain.len(), 32);
        assert_eq!(chain.last(), Some(&BindingIdentity(0)));
    }

    #[test]
    fn traversal_stops_at_a_cycle() {
        let edges = HashMap::from([
            (BindingIdentity(1), BindingIdentity(2)),
            (BindingIdentity(2), BindingIdentity(1)),
        ]);
        assert_eq!(
            origin_chain(BindingIdentity(1), |identity| edges.get(&identity).copied()),
            vec![BindingIdentity(2)]
        );
    }
}
