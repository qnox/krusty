//! Which value a flow proof is about, and how a later read finds that proof again.
//!
//! A narrowing proved by `if (ref != null)` has to be recovered when `ref` is read afterwards, and
//! the two sites must agree on WHICH value was narrowed. Both build the same [`NarrowPath`] from
//! what their checks selected: the lexical value's flow identity, the receiver-tower coordinate of
//! the receiver a property was read through, the selected property declarations. No step recovers
//! a binding, receiver or property from its spelling, so a proof about one receiver's property is
//! never consumed by a read of a same-named property of a nearer receiver.

use super::scope::{PathProperty, PathRoot};
use super::*;

impl Checker<'_> {
    /// The access path a checked expression denotes: the value its name read selected, followed by
    /// the member properties selected through plain (`.`) and safe (`?.`) reads. `None` for
    /// anything else (a call result, an indexed read, a temporary) — a proof on it says nothing
    /// about a later re-read.
    pub(super) fn expr_access_path(&self, e: ExprId) -> Option<NarrowPath> {
        match self.file.expr(e) {
            Expr::Name(_) => {
                if let Some(root) = self.read_flow_roots.get(&e) {
                    return Some(NarrowPath::root_only(root.clone()));
                }
                match self.expr_lowers.get(&e)? {
                    ExprLowering::MemberPropertyRead { .. } => {
                        let receiver = *self.implicit_receiver_identities.get(&e)?;
                        Some(
                            NarrowPath::root_only(PathRoot::Receiver(receiver))
                                .then(self.selected_path_property(e)?),
                        )
                    }
                    ExprLowering::LabeledThisInner | ExprLowering::LabeledThisDispatch => {
                        let receiver = *self.implicit_receiver_identities.get(&e)?;
                        Some(NarrowPath::root_only(PathRoot::Receiver(receiver)))
                    }
                    ExprLowering::TopLevelPropertyGet(access) => Some(NarrowPath::root_only(
                        PathRoot::TopLevel(self.top_level_path_property(&access.property)),
                    )),
                    _ => None,
                }
            }
            Expr::Member { receiver, .. }
            | Expr::SafeCall {
                receiver,
                args: None,
                ..
            } => Some(
                self.expr_access_path(*receiver)?
                    .then(self.selected_path_property(e)?),
            ),
            Expr::As {
                operand,
                nullable: false,
                ..
            } => self.expr_access_path(*operand),
            _ => None,
        }
    }

    /// The member property a checked read selected.
    fn selected_path_property(&self, read: ExprId) -> Option<PathProperty> {
        match self.expr_lowers.get(&read)? {
            ExprLowering::MemberPropertyRead {
                owner,
                name,
                declaration_ty,
                ..
            } => Some(PathProperty {
                owner: *owner,
                name: name.clone(),
                ty: *declaration_ty,
                stable: self.stable_property_reads.contains(&read),
            }),
            _ => None,
        }
    }

    /// A selected top-level property as a path root. A same-file `val` with its compiler-default
    /// backing-field getter is stable like a local `val`; cross-file, computed and delegated
    /// properties remain accessor reads.
    fn top_level_path_property(&self, property: &crate::libraries::PropertyInfo) -> PathProperty {
        let source_file = property.source_key.map(|(file, _)| file).or_else(|| {
            let declaration = property.stable_declaration?;
            self.resolved_index?
                .declaration_anchor(declaration)
                .map(|anchor| anchor.source.raw())
        });
        PathProperty {
            owner: property.owner,
            name: property.name.clone(),
            ty: property.ty,
            stable: property.context_count == 0
                && source_file == Some(self.file_index)
                && property.read_stability == crate::libraries::PropertyReadStability::Stable,
        }
    }

    /// The path of the receiver `this` currently denotes.
    pub(super) fn current_receiver_path(&self, scope: &CheckerScope<'_>) -> Option<NarrowPath> {
        let receiver = self.declared_implicit_receivers(scope).into_iter().next()?;
        Some(NarrowPath::root_only(PathRoot::Receiver(receiver.identity)))
    }

    /// The path of the lexical value `name` currently denotes.
    pub(super) fn visible_value_path(
        &self,
        scope: &CheckerScope<'_>,
        name: &str,
    ) -> Option<NarrowPath> {
        let local = self.lookup(scope, name)?;
        Some(NarrowPath::root_only(PathRoot::Value(local.flow_identity)))
    }

    /// The path of the value the checker selected to fill a context parameter: the exact lexical
    /// binding at its recorded shadow depth, or the current receiver.
    pub(super) fn context_argument_path(
        &self,
        scope: &CheckerScope<'_>,
        source: &ResolvedContextArgument,
    ) -> Option<NarrowPath> {
        match source {
            ResolvedContextArgument::Binding { name, shadow_depth } => {
                let local = scope
                    .shadowed_binding(Ns::Value, name, *shadow_depth)?
                    .value()?;
                Some(NarrowPath::root_only(PathRoot::Value(local.flow_identity)))
            }
            ResolvedContextArgument::ImplicitReceiver(selection) if selection.current => {
                self.current_receiver_path(scope)
            }
            ResolvedContextArgument::ImplicitReceiver(_) => None,
        }
    }

    /// The visible lexical binding carrying flow identity `identity`, with the name it is visible
    /// under. A same-named nearer declaration hides it.
    pub(super) fn visible_flow_value(
        &self,
        scope: &CheckerScope<'_>,
        identity: u32,
    ) -> Option<(String, Local)> {
        let mut found = None;
        scope.visit_bindings(Ns::Value, |name, binding| {
            if found.is_none() {
                if let Some(local) = binding
                    .value()
                    .filter(|local| local.flow_identity == identity)
                {
                    found = Some((name.to_string(), local));
                }
            }
        });
        let (name, local) = found?;
        (self.lookup(scope, &name)?.flow_identity == identity).then_some((name, local))
    }

    /// Apply one narrowing without the support gate: a root-only lexical path shadows the binding
    /// in the current scope (reads resolve to the narrowed `Local`); every other path is recorded
    /// in the current flow frame for the read-time hooks.
    pub(super) fn apply_narrowing_unchecked(
        &mut self,
        scope: &CheckerScope<'_>,
        path: &NarrowPath,
        ty: Ty,
    ) {
        if let (PathRoot::Value(identity), true) = (&path.root, path.segments.is_empty()) {
            if let Some((name, _)) = self.visible_flow_value(scope, *identity) {
                crate::trace_compiler!("smartcast", "root narrowing shadows lexical value {name}");
                self.declare_narrowing_shadow(scope, &name, ty);
            }
            return;
        }
        self.record_path_narrowing(scope, path.clone(), ty);
        // A member property of a class body is also bound in the lexical namespace, so its bare
        // read resolves through that binding. The proof reaches it when the binding carries this
        // very receiver and declaration.
        if let (PathRoot::Receiver(receiver), [property]) = (&path.root, path.segments.as_slice()) {
            let carries_property = self.lookup(scope, &property.name).is_some_and(|local| {
                matches!(
                    local.origin,
                    ReceiverFnValueOrigin::DispatchProperty {
                        owner,
                        receiver_identity,
                        ..
                    } if owner == property.owner && receiver_identity == *receiver
                )
            });
            if carries_property {
                self.declare_narrowing_shadow(scope, &property.name, ty);
            }
        }
    }

    /// The type a proof gives a member property read through an explicit receiver: the proof
    /// holds while the path's stable type is still the declared one.
    pub(super) fn path_narrowed_read_ty(
        &self,
        scope: &CheckerScope<'_>,
        read: ExprId,
        receiver: ExprId,
        declared: Ty,
    ) -> Ty {
        let Some(path) = self.expr_access_path(read) else {
            return declared;
        };
        self.proven_read_ty(scope, &path, self.span(receiver), declared)
    }

    /// The type a proof gives a bare member property read through the implicit receiver `receiver`
    /// selected for it.
    pub(super) fn receiver_property_narrowed_read_ty(
        &self,
        scope: &CheckerScope<'_>,
        read: ExprId,
        receiver: (usize, usize),
        declared: Ty,
    ) -> Ty {
        let Some(property) = self.selected_path_property(read) else {
            return declared;
        };
        let path = NarrowPath::root_only(PathRoot::Receiver(receiver)).then(property);
        self.proven_read_ty(scope, &path, self.span(read), declared)
    }

    fn proven_read_ty(
        &self,
        scope: &CheckerScope<'_>,
        path: &NarrowPath,
        site: Span,
        declared: Ty,
    ) -> Ty {
        let Some(narrowed) = self.lookup_path_narrowing(scope, path) else {
            return declared;
        };
        let current = self.stable_path_ty(scope, path, site);
        let still_valid = current.is_some_and(|current| current.non_null() == declared.non_null());
        crate::trace_compiler!(
            "smartcast",
            "read path={path:?} declared={declared:?} narrowed={narrowed:?} current={current:?} valid={still_valid}",
        );
        if narrowed != declared && still_valid {
            narrowed
        } else {
            declared
        }
    }

    /// The nearest proof recorded for `path`. The path's identities already name one value, so
    /// the walk needs no binding or receiver boundary.
    pub(super) fn lookup_path_narrowing(
        &self,
        scope: &CheckerScope<'_>,
        path: &NarrowPath,
    ) -> Option<Ty> {
        scope.ancestors().find_map(|rung| rung.path_narrowing(path))
    }

    pub(super) fn top_level_property_read_ty(
        &self,
        scope: &CheckerScope<'_>,
        property: &crate::libraries::PropertyInfo,
        site: Span,
    ) -> Ty {
        let declared = property.ty;
        let path =
            NarrowPath::root_only(PathRoot::TopLevel(self.top_level_path_property(property)));
        crate::trace_compiler!(
            "smartcast",
            "top-level read candidate path={path:?} declared={declared:?}",
        );
        let Some(narrowed) = self.lookup_path_narrowing(scope, &path) else {
            return declared;
        };
        let stable = self.stable_path_ty(scope, &path, site);
        crate::trace_compiler!(
            "smartcast",
            "top-level read path={path:?} declared={declared:?} narrowed={narrowed:?} stable={stable:?}",
        );
        if stable == Some(declared) {
            narrowed
        } else {
            declared
        }
    }

    /// All negative facts currently proved for one stable path. The path's identities name one
    /// value, so the walk needs no binding or receiver boundary.
    pub(super) fn lookup_flow_exclusions(
        &self,
        scope: &CheckerScope<'_>,
        path: &NarrowPath,
    ) -> Vec<FlowExclusion> {
        let mut exclusions = Vec::new();
        for rung in scope.ancestors() {
            for exclusion in rung.exclusions(path) {
                if !exclusions.contains(&exclusion) {
                    exclusions.push(exclusion);
                }
            }
        }
        exclusions
    }

    /// All incomparable smart-cast constituents currently proved for one access path. Like the
    /// ordinary path-narrowing lookup, the path's identities bound the walk. The vector is
    /// body-flow state only: callers project one constituent for a concrete semantic operation, so
    /// no synthetic intersection identity can escape into FIR.
    pub(super) fn lookup_intersection_narrowing(
        &self,
        scope: &CheckerScope<'_>,
        path: &NarrowPath,
    ) -> Vec<Ty> {
        scope
            .ancestors()
            .map(|rung| rung.intersection_narrowing(path))
            .find(|types| !types.is_empty())
            .unwrap_or_default()
    }
}
