//! What one inline lowering did with a call, as the call's emission reads it.
//!
//! A lowering may decline a call only before it commits to a placement plan and before any of the
//! call's code is emitted. Candidate literal positions may already be known while the host plan is
//! probed, but a literal that cannot legally exist as a real callable (a non-local jump) and an
//! `@InlineOnly` target make that probe mandatory: an unsupported host is then an error, not a
//! decline. Once a lowering owns the call, a failure is recorded as the run's error and the caller
//! never lowers it another way.

/// The result of one inline lowering of a call.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[must_use]
pub(super) enum InlineCallOutcome {
    /// The lowering does not apply. Decided before a placement plan was committed or any code was
    /// emitted, and every literal can legally be materialized, so the caller may lower the call
    /// another way.
    NotApplicable,
    /// The lowering emitted the call.
    Handled,
    /// The lowering owned the call and failed; the reason is the run's recorded error. The caller
    /// emits nothing more for the call.
    HandledWithError,
}

impl InlineCallOutcome {
    /// A lowering of a call without selected literals, which either declines before emitting
    /// anything (`None`) or emits the call.
    pub(super) fn declined_or_handled(emitted: Option<()>) -> Self {
        match emitted {
            Some(()) => Self::Handled,
            None => Self::NotApplicable,
        }
    }

    /// Whether the lowering owned the call, successfully or not.
    pub(super) fn owns_call(self) -> bool {
        !matches!(self, Self::NotApplicable)
    }
}
