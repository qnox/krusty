//! The arity decision for an arrow-less literal's implicit `it`.

use super::*;

/// Which `it` an arrow-less literal's reads name. kotlinc gives the literal its own parameter
/// only when its expected function type supplies one value parameter; otherwise `it` lexically
/// names the enclosing literal's parameter.
#[derive(Clone, Copy, Debug)]
pub(super) enum ImplicitItDecision {
    /// The expected function type supplies the literal's own parameter, of this type.
    Own(ResolvedTy),
    /// No own parameter, and the body reads `it`: it names the enclosing literal's.
    Enclosing,
    /// No own parameter, and the body never reads `it`.
    Unread,
}

impl ImplicitItDecision {
    /// The decision for a literal whose expectation supplies no value parameter.
    pub(super) fn without_own(read: SigExprId, result: SigExprId, graph: &SignatureGraph) -> Self {
        if graph.expression_reaches_any(result, &[read]) {
            Self::Enclosing
        } else {
            Self::Unread
        }
    }
}
