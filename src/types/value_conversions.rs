//! The conversion a checked use of a value makes when its type differs from the type the use
//! expects, decided from the two types alone.
//!
//! The checker publishes the decision on source expressions, and KLIB dependency body lowering
//! applies the same decision to the implicit casts a serialized body states, so both produce the
//! same common IR for the same pair of types.

use super::{Ty, TyKind};

/// How a value of one type is used as a value of another type the frontend accepted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ValueConversion {
    /// A value used where `Unit` is expected.
    CoerceToUnit,
    /// A numeric value widened to a wider numeric type.
    NumericWidening,
    /// A value of an intersection viewed as one of its constituents.
    ConstituentView,
    /// A value used as a nullable type, or a scalar value used as a reference.
    NullabilityWidening,
    /// A reference value proven to have a scalar type.
    SmartCast,
}

/// The conversion a value of `actual` makes when it is used as a value of `target`, or `None`
/// when the use needs no conversion of its own: the types are one type, the value is `Nothing`
/// or `null`, or both are non-null references (a narrowing of which is a cast of the value).
pub fn value_conversion(actual: Ty, target: Ty) -> Option<ValueConversion> {
    let actual = actual.canonical_semantic();
    let target = target.canonical_semantic();
    if actual == target || matches!(actual, Ty::Nothing | Ty::Null) {
        return None;
    }
    Some(if target == Ty::Unit {
        ValueConversion::CoerceToUnit
    } else if target.accepts_numeric(actual) {
        ValueConversion::NumericWidening
    } else if matches!(
        actual.non_null().kind(),
        TyKind::Intersection(parts) if parts
            .iter()
            .any(|part| part.non_null() == target.non_null())
    ) {
        ValueConversion::ConstituentView
    } else if target.is_nullable() || (!actual.is_reference() && target.is_reference()) {
        ValueConversion::NullabilityWidening
    } else if actual.is_reference() && !target.is_reference() {
        ValueConversion::SmartCast
    } else {
        return None;
    })
}
