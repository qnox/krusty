//! The semantic mode of a source `==` or `!=`.

use super::Ty;

/// The semantic mode selected when the frontend checks a source `==` or `!=`.
///
/// Inline substitution preserves this decision even when it changes the operands' physical
/// representation. Common IR carries the mode and each backend realizes it without reclassifying
/// the substituted operand types.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EqualityMode {
    /// Kotlin structural equality, including boxed floating-point `equals` semantics.
    Structural,
    /// Primitive IEEE-754 equality of a `Float` or `Double` pair.
    Ieee754,
    /// Primitive equality of scalar operands that are not a floating-point pair.
    Primitive,
}

impl EqualityMode {
    /// The mode of a source equality whose operands have the types written at the comparison,
    /// before inline substitution: IEEE 754 equality for two operands of one floating-point type,
    /// primitive equality for any other two scalar values, and structural equality otherwise.
    pub fn of_source_operands(lhs: Ty, rhs: Ty) -> Self {
        let left = lhs.canonical_semantic().scalar_value_repr();
        let right = rhs.canonical_semantic().scalar_value_repr();
        match (left, right) {
            (Some(left), Some(right))
                if matches!(left, Ty::Float | Ty::Double) && left == right =>
            {
                Self::Ieee754
            }
            (Some(_), Some(_)) => Self::Primitive,
            _ => Self::Structural,
        }
    }
}
