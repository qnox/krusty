//! The expected-result relation carried through compact signature-call evaluation.

use super::ResolvedTy;

/// How the enclosing expression exposes a selected callable's declaration result.
///
/// A safe call contextualizes the nullable expression result, not the declaration result itself.
/// Keeping that boundary explicit prevents signature evaluation from treating `R` and `R?` as the
/// same inference constraint when an outer call supplies an expected type.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SignatureCallResultConstraint {
    Direct(ResolvedTy),
    SafeLifted(ResolvedTy),
}

impl SignatureCallResultConstraint {
    pub fn expected(self) -> ResolvedTy {
        match self {
            Self::Direct(expected) | Self::SafeLifted(expected) => expected,
        }
    }

    pub(super) fn safe_lifted(self) -> Self {
        Self::SafeLifted(self.expected())
    }
}
