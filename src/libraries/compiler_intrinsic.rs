//! Compiler-supplied realization identities attached to selected declarations.

use super::{PrimitiveBinaryIntrinsic, PrimitiveUnaryIntrinsic};

/// A source-declared callable whose implementation is supplied by the compiler after ordinary symbol
/// and overload selection. Providers attach this to the exact declaration identity; lowering never
/// grants intrinsic behavior from a coincidental source name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CompilerIntrinsic {
    /// Exact Kotlin array-factory declaration whose implementation is supplied by the compiler.
    /// The kind distinguishes reference/primitive varargs and the other language array creators;
    /// selected call-site types remain ordinary semantic types and are recorded in checked FIR.
    ArrayFactory(crate::types::ArrayFactoryKind),
    /// An array classifier's own `operator fun get(index: Int)` (`IntArray.get`, `Array<T>.get`).
    /// The array classifiers have no platform class, so a target reads the element directly.
    ArrayGet,
    /// An array classifier's own `operator fun set(index: Int, value)`, realized as a direct
    /// element store for the same reason as [`Self::ArrayGet`].
    ArraySet,
    ArraySize,
    CharCode,
    StringLength,
    /// `String.get(Int): Char`, the index operator on a string.
    StringGet,
    StringPlus,
    NullableAnyToString,
    /// Exact builtin numeric/character conversion declaration selected from Kotlin builtins.
    /// The source and target types remain call-site semantic facts on FIR; this marker only states
    /// that the selected declaration is realized as a conversion rather than virtual dispatch.
    NumericConversion,
    /// Exact builtin scalar unary declaration. The selected result names the promoted primitive
    /// carrier (`Byte.unaryPlus(): Int`); checked FIR records any receiver conversion before
    /// publishing the identity/negation operation.
    PrimitiveUnary(PrimitiveUnaryIntrinsic),
    /// Exact primitive bit operation selected from Kotlin builtins. These declarations have no
    /// callable JVM implementation: checked FIR publishes the operation and the backend emits the
    /// target's primitive instruction.
    PrimitiveBitAnd,
    PrimitiveBitOr,
    PrimitiveBitXor,
    PrimitiveShiftLeft,
    PrimitiveShiftRight,
    PrimitiveUnsignedShiftRight,
    PrimitiveBitNot,
    /// `hashCode()` as a builtin scalar class declares it (`Int.hashCode(): Int`). kotlinc's `HashCode`
    /// intrinsic hashes the unboxed value through its wrapper's static `hashCode`, never boxing it.
    PrimitiveHashCode,
    /// Exact builtin `Boolean.not()` declaration. Kotlin exposes it as an ordinary member, but the
    /// target realizes logical negation directly because no platform method implements it.
    BooleanNot,
    /// Arithmetic member selected from an exact builtin scalar declaration (`Int.plus`,
    /// `Double.rem`, and their mixed-operand overloads). Kotlin publishes these as semantic members,
    /// but the JVM has no corresponding virtual method; checked FIR turns the provider fact into its
    /// source-level binary operation after overload selection has fixed the declaration.
    PrimitiveBinary(PrimitiveBinaryIntrinsic),
    /// Exact builtin scalar `compareTo` declaration. Its selected receiver and parameter determine
    /// the common comparison carrier; checked FIR publishes that carrier so backends implement the
    /// declaration without inventing a virtual wrapper method.
    PrimitiveCompare,
    Assert,
    AssertFailsWith,
    Print,
    Println,
    StartCoroutine,
    /// The current suspend body's continuation context. The stdlib declaration is a public
    /// `@InlineOnly` suspend property whose private throwing accessor is never invoked directly.
    CoroutineContext,
    CoroutineSuspended,
    SuspendCoroutine,
    SuspendCoroutineUninterceptedOrReturn,
    EnumValues,
    EnumValueOf,
    /// The exact `kotlin.Enum.name` declaration published by the core provider. A direct enum entry
    /// folds to that entry's declaration name; any other receiver stays an ordinary property call.
    EnumName,
    /// `kotlin.reflect.typeOf<T>()`. The stdlib body only throws: the selected type argument is
    /// the whole operand, and each target builds its runtime `KType` from that semantic type.
    TypeOf,
    IsEmpty,
    IsNotEmpty,
    Count,
    TrimIndent,
    TrimMargin,
    /// The catalog `kotlin.ranges` `Double`/`Float` `rangeTo` and `rangeUntil` declarations.
    /// Membership is an IEEE comparison. The provider copies this role from the exact catalog
    /// declaration onto its physical realization; a nearer operator and an otherwise identical
    /// noncatalog declaration do not carry it.
    FloatingRangeMembership,
    /// `kotlin.ranges` progression builders a counted `for` loop reads through instead of calling
    /// (kotlinc's `ForLoopsLowering` handlers). Outside a loop header they are ordinary calls.
    RangeDownTo,
    RangeUntil,
    ProgressionStep,
    ProgressionReversed,
    /// The stdlib's unsigned comparison over the carrier of an unsigned value class
    /// (`uintCompare(Int, Int): Int`, `ulongCompare(Long, Long): Int`). kotlinc's counted loops
    /// call it to order unsigned bounds; a source call of it is an ordinary call.
    UnsignedCompare {
        carrier: crate::types::Ty,
    },
    /// `next()` declared by a Kotlin primitive iterator (`kotlin.collections.IntIterator.next`).
    /// A target may call the iterator's unboxed element operation instead of the boxed
    /// `Iterator<T>.next` bridge (kotlinc's `IteratorNext` intrinsic); outside that target rule the
    /// declaration is ordinary virtual dispatch.
    PrimitiveIteratorNext,
}
