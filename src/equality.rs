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
