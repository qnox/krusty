//! The semantic conversion a value needs to cross into a slot of another type.
//!
//! Kotlin admits a value of a subtype wherever its supertype is expected. Most such crossings
//! keep the value as it is; the ones below change how the value is viewed or represented, and
//! common IR records each as an explicit conversion. The checker publishes the decision for a
//! source value, and a serialized dependency body lowers its operands by the same decision.

use super::{Ty, TyKind};

/// How a value of one semantic type crosses into a slot of another.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ValueConversion {
    /// Into `Unit`: the value is evaluated for its effect.
    CoerceToUnit,
    /// A numeric value into a wider numeric type or a numeric supertype.
    NumericWidening,
    /// One constituent of an intersection, the view resolution selected a declaration on.
    IntersectionView,
    /// Into a nullable type, or a primitive into a reference type.
    NullabilityWidening,
    /// A reference into a non-reference type.
    ReferenceNarrowing,
}

/// The conversion a value of `actual` needs in a slot of `target`, or `None` when the value
/// crosses unchanged: identical types, a bottom value, or a reference into a non-null reference
/// supertype.
pub fn value_conversion(actual: Ty, target: Ty) -> Option<ValueConversion> {
    let actual = actual.canonical_semantic();
    let target = target.canonical_semantic();
    if actual == target || matches!(actual, Ty::Nothing | Ty::Null) {
        return None;
    }
    if target == Ty::Unit {
        Some(ValueConversion::CoerceToUnit)
    } else if target.accepts_numeric(actual) {
        Some(ValueConversion::NumericWidening)
    } else if matches!(
        actual.non_null().kind(),
        TyKind::Intersection(parts) if parts
            .iter()
            .any(|part| part.non_null() == target.non_null())
    ) {
        Some(ValueConversion::IntersectionView)
    } else if target.is_nullable() || (!actual.is_reference() && target.is_reference()) {
        Some(ValueConversion::NullabilityWidening)
    } else if actual.is_reference() && !target.is_reference() {
        Some(ValueConversion::ReferenceNarrowing)
    } else {
        None
    }
}
