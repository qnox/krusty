//! Checked direct-supertype state for classifiers owned by the active Pass-2 body.
//!
//! The immutable module provider cannot expose a deferred local header while its lexical class
//! rung is still being checked. This bounded overlay owns that hierarchy until the unit closes.

use super::*;

impl Checker<'_> {
    /// Retain a fully checked direct-supertype edge for active body-local lookup. An empty entry is
    /// authoritative: the active local owner has no direct edges. The first exact lexical result
    /// wins; reopened or partial checks must not replace it.
    pub(super) fn record_checked_body_local_supertypes(
        &mut self,
        owner: Option<TypeName>,
        checked: &[Ty],
    ) {
        let Some(owner) = owner.filter(|owner| {
            checked.iter().all(|supertype| {
                crate::fir::ResolvedTy::new(*supertype).is_ok()
                    && supertype.kotlin_class_internal().is_some()
                    && supertype.kotlin_class_internal() != Some(*owner)
            })
        }) else {
            return;
        };
        self.resolved_body_local_supertypes
            .entry(owner)
            .or_insert_with(|| checked.into());
    }

    /// Applied Pass-2 supertypes of a local/anonymous classifier receiver. The stored edges are in
    /// declaration form; specialize them when the receiver applies the local classifier's own or
    /// captured parameters.
    pub(super) fn body_local_supertypes(&self, receiver: Ty) -> Vec<Ty> {
        let Some(internal) = receiver.non_null().kotlin_class_internal() else {
            return Vec::new();
        };
        let Some(declared) = self.resolved_body_local_supertypes.get(&internal) else {
            return Vec::new();
        };
        let deferred_body_local = self
            .resolved_index
            .and_then(|index| {
                index
                    .classifier_declaration(internal)
                    .map(|declaration| index.classifier_header(declaration).is_none())
            })
            .unwrap_or(false);
        let active_bindings = || {
            // A deferred body-local header is intentionally absent from the module provider
            // while its class body is being checked. Its type-parameter identities belong to
            // the live parser unit and are already resolved on that lexical declaration.
            let formals = self
                .resolved_index
                .and_then(|index| index.classifier_declaration(internal))
                .and_then(|declaration| {
                    self.active_declarations?
                        .span(self.file, declaration)
                        .map(|span| span.lo)
                })
                .map(|start| {
                    self.resolved_declaration_type_parameters
                        .get(&start)
                        .cloned()
                        .unwrap_or_default()
                })
                .unwrap_or_default();
            formals
                .into_iter()
                .zip(receiver.type_args().iter().copied())
                .collect()
        };
        let bindings = if deferred_body_local {
            active_bindings()
        } else {
            self.resolved_type_name(internal)
                .map(|class| crate::symbol_resolver::classifier_bindings(&class, receiver))
                .unwrap_or_else(active_bindings)
        };
        declared
            .iter()
            .map(|supertype| crate::symbol_resolver::ty_subst_keep_unbound(*supertype, &bindings))
            .collect()
    }
}
