//! Primitive values reaching reference supertypes through their Kotlin declaration hierarchy.

use crate::assignable::{is_subtype, TyCtx, TypeOracle};
use crate::types::{Ty, TyKind};

/// Preserve applied supertypes: Int reaches Comparable<Int>, not Comparable<String>. The
/// primitive classifier is already a Kotlin identity; no platform wrapper participates.
pub(super) fn reaches_reference(oracle: &dyn TypeOracle, actual: Ty, expected: Ty) -> bool {
    let scalar = actual.is_numeric_or_char() || actual == Ty::Boolean || actual.is_unsigned();
    scalar
        && matches!(expected.kind(), TyKind::Obj(..))
        && is_subtype(&TyCtx::new(), oracle, actual, expected)
}
