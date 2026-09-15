//! Expression-owned transformation of a selected callable result before contextual inference.

use std::borrow::Cow;

use crate::libraries::GenericSig;
use crate::types::Ty;

/// The relation between a declaration return and the type expected for the whole call expression.
///
/// Ordinary calls expose the declaration return directly. A safe call exposes its nullable lift;
/// keeping that transform explicit prevents callers from guessing by stripping nullability from
/// the expectation, which cannot distinguish `R` from an already-nullable `R?`.
#[derive(Clone, Copy, Debug)]
pub(super) enum CallResultConstraint {
    None,
    Direct(Ty),
    SafeLifted(Ty),
}

impl CallResultConstraint {
    pub(super) fn direct(expected: Option<Ty>) -> Self {
        expected.map_or(Self::None, Self::Direct)
    }

    pub(super) fn safe_lifted(expected: Option<Ty>) -> Self {
        expected.map_or(Self::None, Self::SafeLifted)
    }

    pub(super) fn expected(self) -> Option<Ty> {
        match self {
            Self::None => None,
            Self::Direct(expected) | Self::SafeLifted(expected) => Some(expected),
        }
    }

    /// Return the declaration signature as observed at the expression boundary. Only the return is
    /// transformed; parameters, formals, and bounds remain the selected declaration contract.
    pub(super) fn signature(self, signature: &GenericSig) -> Cow<'_, GenericSig> {
        match self {
            Self::SafeLifted(_) => {
                let mut lifted = signature.clone();
                lifted.ret = Ty::nullable(lifted.ret);
                Cow::Owned(lifted)
            }
            Self::None | Self::Direct(_) => Cow::Borrowed(signature),
        }
    }

    /// Apply the expression transform to a non-signature return, used by member-extension
    /// instantiation before it unifies the result with the contextual expectation.
    pub(super) fn declaration_result(self, declared: Ty) -> Ty {
        match self {
            Self::SafeLifted(_) => Ty::nullable(declared),
            Self::None | Self::Direct(_) => declared,
        }
    }

    /// The contextual approximation corresponding to the selected callable's own result. This is
    /// used only when input constraints solved that result to bottom: a safe-call boundary is
    /// removed for a non-null declaration, but an explicitly nullable declaration retains it.
    pub(super) fn selected_result_approximation(self, declared: Ty) -> Option<Ty> {
        match self {
            Self::None => None,
            Self::Direct(expected) => Some(expected),
            Self::SafeLifted(expected) if declared.admits_null() => Some(expected),
            Self::SafeLifted(expected) => Some(expected.non_null()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::CallResultConstraint;
    use crate::libraries::{GenericReturnPolicy, GenericSig};
    use crate::types::Ty;

    fn signature(ret: Ty) -> GenericSig {
        GenericSig {
            formals: vec!["T".to_string()],
            formal_bounds: vec![vec![Ty::nullable(Ty::obj("kotlin/Any"))]],
            params: Vec::new(),
            ret,
            receiver: None,
            return_policy: GenericReturnPolicy::Exact,
        }
    }

    #[test]
    fn safe_lift_is_idempotent_for_an_already_nullable_return() {
        let nullable = Ty::nullable(Ty::ty_param("T", Ty::nullable(Ty::obj("kotlin/Any"))));
        let signature = signature(nullable);
        let lifted =
            CallResultConstraint::safe_lifted(Some(Ty::nullable(Ty::String))).signature(&signature);
        assert_eq!(lifted.ret, nullable);
    }

    #[test]
    fn safe_lift_wraps_a_non_null_declaration_return() {
        let signature = signature(Ty::ty_param("T", Ty::obj("kotlin/Any")));
        let lifted =
            CallResultConstraint::safe_lifted(Some(Ty::nullable(Ty::String))).signature(&signature);
        assert_eq!(lifted.ret, Ty::nullable(signature.ret));
    }

    #[test]
    fn safe_result_approximation_removes_only_the_boundary_lift() {
        let constraint = CallResultConstraint::safe_lifted(Some(Ty::nullable(Ty::String)));
        assert_eq!(
            constraint.selected_result_approximation(Ty::ty_param(
                "T",
                Ty::nullable(Ty::obj("kotlin/Any")),
            )),
            Some(Ty::String),
        );
    }

    #[test]
    fn safe_result_approximation_preserves_declared_nullability() {
        let constraint = CallResultConstraint::safe_lifted(Some(Ty::nullable(Ty::String)));
        assert_eq!(
            constraint.selected_result_approximation(Ty::nullable(Ty::ty_param(
                "T",
                Ty::nullable(Ty::obj("kotlin/Any")),
            ))),
            Some(Ty::nullable(Ty::String)),
        );
    }
}
