//! Which object a flow proof is about, and how a later read finds that proof again.
//!
//! A narrowing proved by `if (ref != null)` has to be recovered when `ref` is read afterwards, and
//! the two sites must agree on WHICH object was narrowed. A `this`-rooted path names a property of
//! one specific receiver, and a `this` rebind — a receiver lambda, an inner class, an extension
//! receiver, even to the same type — is a different object, so the proof lives in the frame of the
//! scope that established that receiver and the lookup stops walking outward there.
//!
//! The receiver walk is shared between recording a proof and reading one back: a bare name must
//! resolve against the same receiver in both, or the two selections can disagree and a proof is
//! recorded about one object and consumed as another.

use super::*;

impl Checker<'_> {
    /// The access path an expression denotes: a root name followed by property segments through
    /// plain (`.`) and safe (`?.`) member reads. `None` for anything else (a call result, an
    /// indexed read, a temporary) — a proof on it says nothing about a later re-read.
    pub(super) fn expr_access_path(&self, e: ExprId) -> Option<NarrowPath> {
        match self.file.expr(e) {
            Expr::Name(n) => Some(NarrowPath::root_only(n)),
            Expr::Member { receiver, name } => {
                let mut path = self.expr_access_path(*receiver)?;
                path.segments.push(name.clone());
                Some(path)
            }
            Expr::SafeCall {
                receiver,
                name,
                args: None,
            } => {
                let mut path = self.expr_access_path(*receiver)?;
                path.segments.push(name.clone());
                Some(path)
            }
            Expr::As {
                operand,
                nullable: false,
                ..
            } => self.expr_access_path(*operand),
            _ => None,
        }
    }

    /// Apply one narrowing without the support gate: a root-only path shadows the binding in the
    /// current scope (the classic mechanism — reads resolve to the narrowed `Local`), a property
    /// path is recorded in the current [`Self::path_narrows`] frame for the read-time hook.
    pub(super) fn apply_narrowing_unchecked(
        &mut self,
        scope: &CheckerScope<'_>,
        path: &NarrowPath,
        ty: Ty,
    ) {
        if path.segments.is_empty() {
            let root = path.root.clone();
            if root == "this" {
                // `this` is a receiver coordinate, not a top-level property or lexical local. Its
                // active type is carried by `with_this_narrow`; retain the path fact as well for
                // explicit receiver reads and intersection projections.
                self.record_path_narrowing(scope, path.clone(), ty);
                return;
            }
            // Alias: a proof on the BARE name of an own member `val` (`if (p != null)`) also
            // narrows the qualified `this.p` read form — same immutable property.
            if matches!(
                self.lookup(scope, &root).map(|local| local.origin),
                Some(ReceiverFnValueOrigin::DispatchProperty { .. })
            ) {
                self.record_path_narrowing(
                    scope,
                    NarrowPath {
                        root: "this".to_string(),
                        segments: vec![root.clone()],
                    },
                    ty,
                );
            }
            if self.lookup(scope, &root).is_some() {
                crate::trace_compiler!("smartcast", "root narrowing shadows lexical value {root}");
                self.declare_narrowing_shadow(scope, &root, ty);
            } else if self
                .receiver_owning(scope, &root)
                .is_some_and(|receiver| receiver.current)
            {
                // A bare name that is a property of the CURRENT receiver — an extension function
                // reading its own receiver's property — has no lexical binding to shadow, and the
                // READ resolves through that receiver as `this.<name>`. Filing the proof under the
                // bare spelling alone put it under a key the read never asks for, so the narrowing
                // was recorded and then never found. Record the receiver-qualified path the read
                // uses; the bare path stays for spellings that consult it directly.
                self.record_path_narrowing(
                    scope,
                    NarrowPath {
                        root: "this".to_string(),
                        segments: vec![root.clone()],
                    },
                    ty,
                );
                self.record_path_narrowing(scope, path.clone(), ty);
                crate::trace_compiler!(
                    "smartcast",
                    "root narrowing records receiver property this.{root}"
                );
            } else {
                // A stable top-level `val` has no lexical value binding to shadow. Retain its exact
                // access path and let the selected property read consume the proven type.
                self.record_path_narrowing(scope, path.clone(), ty);
                crate::trace_compiler!(
                    "smartcast",
                    "root narrowing records property path {path:?}"
                );
            }
        } else {
            self.record_path_narrowing(scope, path.clone(), ty);
            // Alias: a proof on `this.p` also narrows the BARE `p` read form (same property).
            if path.root == "this" && path.segments.len() == 1 {
                let name = path.segments[0].clone();
                if matches!(
                    self.lookup(scope, &name).map(|local| local.origin),
                    Some(ReceiverFnValueOrigin::DispatchProperty { .. })
                ) {
                    self.declare_narrowing_shadow(scope, &name, ty);
                }
            }
        }
    }

    /// Nearest implicit receiver that declares `name`, if any.
    ///
    /// Scope-tower order is preserved, so an inner receiver shadows an outer one exactly as member
    /// selection does. Both the narrowing RECORDER and the stable-path READ resolve a bare name
    /// through this one walk: if they disagreed about which receiver owns the name, a proof about an
    /// outer receiver's property could be found by a read of a nearer receiver's property of the
    /// same name.
    pub(super) fn receiver_owning(
        &self,
        scope: &CheckerScope<'_>,
        name: &str,
    ) -> Option<ImplicitReceiver> {
        self.implicit_receivers(scope).into_iter().find(|receiver| {
            self.lookup_prop_name(receiver.ty.non_null(), name)
                .is_some()
        })
    }

    pub(super) fn lookup_path_narrowing(
        &self,
        scope: &CheckerScope<'_>,
        path: &NarrowPath,
    ) -> Option<Ty> {
        let rooted_at_this = path.root == "this";
        for rung in scope.ancestors() {
            if let Some(ty) = rung.path_narrowing(path) {
                return Some(ty);
            }
            if rooted_at_this {
                if matches!(
                    rung.kind(),
                    ScopeKind::Class { .. } | ScopeKind::Function { receiver: Some(_) }
                ) {
                    return None;
                }
            } else if rung.declared_here(&path.root, Ns::Value) {
                // The rung declaring the root was just consulted; a proof recorded further out
                // describes the binding this one shadows.
                return None;
            }
        }
        None
    }
}
