//! Stable identities shared by resolver data-flow analyses for lexical value bindings.
//!
//! A spelling is used only to reach a scope entry. Once selected, flow facts carry the binding's
//! identity so a same-named declaration cannot consume or invalidate another binding's facts.

use super::scope::Ns;
use super::{Checker, CheckerScope, Local};

/// Ephemeral identity of one lexical value during a bounded checker run.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(super) struct BindingIdentity(u32);

impl BindingIdentity {
    pub(super) const fn new(identity: u32) -> Self {
        Self(identity)
    }
}

impl Checker<'_> {
    /// Find the currently visible spelling of one exact lexical binding. A same-named inner value
    /// makes the older binding inaccessible and therefore stops data-flow traversal.
    pub(super) fn visible_local_binding(
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
}
