//! Lifecycle state for one declaration's compact signature constraint.

use super::{DiagnosticId, ResolvedSignature};

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SignatureState {
    Uncomputed,
    Computing,
    Resolved(ResolvedSignature),
    Failed(DiagnosticId),
}
