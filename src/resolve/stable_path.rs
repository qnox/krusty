//! Stability and declared-type reads for smart-cast access paths.

use super::scope::PathRoot;
use super::{Checker, CheckerScope, NarrowPath, ReceiverFnValueOrigin, Span, Ty};

/// Bounded view of the checker facts needed to decide whether an access path can be read twice.
pub(super) struct StablePathRead<'checker, 'source> {
    checker: &'checker Checker<'source>,
}

impl<'checker, 'source> StablePathRead<'checker, 'source> {
    pub(super) fn new(checker: &'checker Checker<'source>) -> Self {
        Self { checker }
    }

    /// The declared type of a stable access path. `None` means some read can change between the
    /// proof and its later use, so retaining a smart cast would be unsound.
    pub(super) fn ty(&self, scope: &CheckerScope<'_>, path: &NarrowPath, site: Span) -> Option<Ty> {
        let mut ty = match &path.root {
            PathRoot::Receiver(identity) => {
                let receiver = self
                    .checker
                    .implicit_receivers(scope)
                    .into_iter()
                    .find(|receiver| receiver.identity == *identity)?;
                // A property of a smart-cast `this` is read through the narrowed receiver.
                if receiver.current {
                    self.checker
                        .actual_this_narrow(scope)
                        .unwrap_or(receiver.ty)
                } else {
                    receiver.ty
                }
            }
            PathRoot::Value(identity) => {
                let (name, local) = self.checker.visible_flow_value(scope, *identity)?;
                if local.is_var
                    && (!path.segments.is_empty()
                        || !matches!(local.origin, ReceiverFnValueOrigin::Local)
                        || self.checker.closure_reassigned_before(&name, site))
                {
                    return None;
                }
                if local.has_unstable_delegated_read() {
                    return None;
                }
                // A top-level property at the root of a longer path re-enters its accessor.
                if !path.segments.is_empty()
                    && !matches!(local.origin, ReceiverFnValueOrigin::Local)
                {
                    return None;
                }
                local.ty
            }
            // A top-level property at the root of a longer path re-enters its accessor.
            PathRoot::TopLevel(property) => {
                return (property.stable && path.segments.is_empty()).then_some(property.ty);
            }
        };
        // Each segment is the declaration its read selected, with that selection's stability.
        for segment in &path.segments {
            ty.non_null().obj_internal()?;
            ty = segment.stable.then_some(segment.ty)?;
        }
        Some(ty)
    }
}
