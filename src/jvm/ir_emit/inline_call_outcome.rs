//! What one inline lowering did with a call, as the call's emission reads it.
//!
//! A lowering may decline a call only while nothing of it is decided: before any literal lambda is
//! selected for placement and before any of the call's code is emitted. Once it owns the call, a
//! failure is recorded as the run's error and the call is finished; the caller never lowers it
//! another way.

/// The result of one inline lowering of a call.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[must_use]
pub(super) enum InlineCallOutcome {
    /// The lowering does not apply. Decided before any literal was selected or any code emitted,
    /// so the caller may lower the call another way.
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
