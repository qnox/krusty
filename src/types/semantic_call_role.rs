/// Semantic role attached by a declaration provider after exact callable selection. A target may
/// consume it when the selected language member has no ordinary dispatch on a chosen
/// representation. The role belongs to the checked cross-phase type contract; it is not provider
/// or target metadata.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SemanticCallRole {
    /// The platform stdlib's size-only boxed-array allocation constructor. Its initializer, when
    /// any, is already explicit in the serialized body surrounding this allocation.
    KotlinArrayAllocation,
    KotlinAnyEquals,
    KotlinAnyHashCode,
    KotlinAnyToString,
    /// The single comparison operation declared by `kotlin.Comparable`.
    KotlinComparableCompareTo,
    /// The invocation operation declared by a compiler-provided Kotlin function classifier.
    KotlinFunctionInvoke,
    /// The `name` property declared by Kotlin's callable-reflection contract.
    KotlinCallableReferenceName,
    /// A property-reference read with this many unbound receivers.
    KotlinPropertyReferenceGet(u8),
    /// A mutable property-reference write with this many unbound receivers.
    KotlinPropertyReferenceSet(u8),
    /// The standard property-reference delegation read with this many forwarded receivers.
    KotlinPropertyReferenceDelegateGet(u8),
    /// The standard property-reference delegation write with this many forwarded receivers.
    KotlinPropertyReferenceDelegateSet(u8),
    /// The standard library iterator declaration whose selected receiver is indexed as a
    /// `CharSequence` by Kotlin's for-loop lowering.
    KotlinCharSequenceIterator,
    /// The compiler-synthesized `copy` declaration of a data class. Source declarations carry the
    /// equivalent stable declaration flag; dependency providers derive this role from Kotlin
    /// metadata's synthesized-member kind while that metadata is still authoritative.
    DataClassCopy,
    /// A stdlib numeric declaration marked `@IntrinsicConstEvaluation` whose operation no other
    /// role or compiler intrinsic names: an unsigned operation or conversion, or `floorDiv`/`mod`.
    /// kotlinc evaluates a call to it whose operands are constants.
    IntrinsicConstNumeric(ConstNumericOperation),
}

/// The numeric operation of an [`SemanticCallRole::IntrinsicConstNumeric`] declaration. Operands
/// and result are read at the call's checked types: an operation on mixed widths computes at the
/// wider operand type, then converts to the declared result.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConstNumericOperation {
    Add,
    Subtract,
    Multiply,
    Divide,
    Remainder,
    /// Division rounding toward negative infinity.
    FloorDivide,
    /// The remainder of [`Self::FloorDivide`], with the divisor's sign.
    Modulo,
    /// `compareTo` between two unsigned values of different widths.
    CompareTo,
    /// A conversion to the declared result type (`toUInt()`, `UInt.toLong()`).
    Conversion,
}
