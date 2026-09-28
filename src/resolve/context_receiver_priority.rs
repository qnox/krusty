//! Where a context-parameter receiver sits in the unqualified name tower.
//!
//! kotlinc's `TowerGroup` puts `ImplicitOrNonLocal` (a package or top-level callable) before
//! `ContextReceiverGroup`. A dispatch or extension receiver stays in `Member`, ahead of both, so
//! `with(Scope) { tag() }` still binds the member. A function-type context receiver
//! (`context(A) () -> R`) is the group that follows top-level declarations: inside that lambda,
//! `foo()` is the contextual top-level `foo` when one applies, and a member such as `substring`
//! only when no top-level candidate does.

use super::*;

pub(super) fn is_context_parameter_receiver(
    scope: &CheckerScope<'_>,
    identity: (usize, usize),
) -> bool {
    scope.implicit_receiver_context(identity).is_some()
}

impl Checker<'_> {
    /// Probe one already-ordered implicit receiver for a bare property name. Scope-tower ordering
    /// stays at the caller; this operation only commits the selected declaration and receiver-local
    /// constant/accessor handoff.
    pub(super) fn read_implicit_receiver_name(
        &mut self,
        scope: &CheckerScope<'_>,
        expression: ExprId,
        name: &str,
        receiver: ImplicitReceiver,
    ) -> Option<Ty> {
        // A member read through an IMPLICIT receiver — a companion's property named unqualified
        // from the class body or a nested class — resolves straight out of the member records, so
        // it reaches neither the bare-name nor the explicit-receiver seam. An undetermined member
        // read here would otherwise be taken as the answer.
        if let Some(resolved) = self
            .demand_member
            .filter(|_| {
                self.lookup_prop_name(receiver.ty, name)
                    .is_some_and(|property| property.0 == Ty::Pending)
            })
            .and_then(|demand| demand(receiver.ty, name, &[]))
        {
            return Some(self.set(expression, resolved));
        }
        if let Ty::Obj(_, _) = receiver.ty {
            if self.lookup_prop_name(receiver.ty, name).is_some() {
                match self.select_property_read(scope, receiver.ty, name) {
                    Ok(Some(selection)) => {
                        if selection.access().is_some_and(|(visibility, owner)| {
                            !self.receiver_property_accessible(visibility, owner, receiver.ty)
                        }) {
                            return None;
                        }
                        return Some(self.record_property_read(scope, Some(expression), selection));
                    }
                    Err(PropertyReadAmbiguity::MissingContext) => {
                        self.diags.error(
                            self.span(expression),
                            format!("No context argument for '{name}' found."),
                        );
                        return Some(Ty::Error);
                    }
                    Err(_) => {
                        self.diags.error(
                            self.span(expression),
                            format!("overload resolution ambiguity for member '{name}'"),
                        );
                        return Some(Ty::Error);
                    }
                    Ok(None) => {}
                }
            }
        }
        // A provider may expose a compile-time constant without an accessor candidate (primitive
        // companion constants are the important case). Consult that payload only after an ordinary
        // property declaration had its chance to resolve and pass accessibility checks. Otherwise a
        // source `const val` could bypass its private/protected property declaration merely because
        // the same declaration also published a foldable value.
        if let Some(owner) = receiver.ty.non_null().obj_internal() {
            if let Some(constant) = self
                .fed_source()
                .classifier(owner)
                .and_then(|classifier| classifier.constants.get(name).cloned())
            {
                let ty = constant.ty;
                self.resolved_constants.insert(expression, constant);
                return Some(ty);
            }
        }
        self.try_member_read(
            scope,
            receiver.ty,
            name,
            self.span(expression),
            Some(expression),
        )
    }

    /// The first context-parameter receiver in `receivers` that has a member `name`.
    pub(super) fn read_context_parameter_receiver(
        &mut self,
        scope: &CheckerScope<'_>,
        expression: ExprId,
        name: &str,
        receivers: &[ImplicitReceiver],
    ) -> Option<Ty> {
        for receiver in receivers.iter().copied() {
            let Some(ty) = self.read_implicit_receiver_name(scope, expression, name, receiver)
            else {
                continue;
            };
            self.mark_implicit_receiver_selection(expression, receiver);
            let ty =
                self.receiver_property_narrowed_read_ty(scope, expression, receiver.identity, ty);
            return Some(self.set(expression, ty));
        }
        None
    }
}
