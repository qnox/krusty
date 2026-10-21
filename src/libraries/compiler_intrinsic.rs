//! Compiler-supplied realization identities attached to selected declarations.

use super::{PrimitiveBinaryIntrinsic, PrimitiveUnaryIntrinsic};

/// The exact library helper used to construct or inspect a failed `Result`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResultFailureIntrinsic {
    Create,
    Check,
    CheckOfClass,
}

/// A source-declared callable whose implementation is supplied by the compiler after ordinary symbol
/// and overload selection. Providers attach this to the exact declaration identity; lowering never
/// grants intrinsic behavior from a coincidental source name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CompilerIntrinsic {
    /// An authenticated platform-stdlib `Throwable()` or `Throwable(String?)` constructor.
    /// The target owns the object's storage, including the supplied nullable message.
    ThrowableConstructor {
        message: bool,
    },
    /// The authenticated `Throwable.message: String?` getter, not a same-named user property.
    ThrowableMessage,
    /// Reinterpret one scalar's bits as the equal-width floating or integral scalar selected by
    /// the authenticated declaration. Checked FIR/IR records both semantic endpoint types.
    ReinterpretBits,
    /// One scalar instruction the target stdlib declares as the callable's implementation. The
    /// authenticated declaration fixes its operand and result types. Checked FIR and common IR
    /// keep its call an ordinary call; only the target emitter replaces it with the instruction.
    ScalarInstruction(super::ScalarInstruction),
    /// Native's exact `GCUnsafeCall` floating-point predicate declaration. The provider
    /// authenticates the runtime entry and complete extension-receiver shape while it still owns
    /// the platform KLIB; target lowering consumes only this operation and scalar type.
    FloatingIsNaN {
        operand: crate::types::Ty,
    },
    /// The corresponding exact infinity predicate declaration.
    FloatingIsInfinite {
        operand: crate::types::Ty,
    },
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
    /// `equals(Any?)` as a builtin scalar class declares it (`Int.equals(Any?): Boolean`). kotlinc's
    /// `ExplicitEquals` intrinsic compares both operands as objects through `Object.equals`.
    PrimitiveEquals,
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
    SuspendCoroutineUninterceptedOrReturn,
    EnumValues,
    EnumValueOf,
    /// The zero-argument `kotlin.enums.enumEntries<T>()`. Its stdlib body only throws; the selected
    /// enum type argument is realized as that enum's `getEntries()`.
    EnumEntries,
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
    /// The authenticated target stdlib's zero-argument `CharSequence.trim(): CharSequence`.
    /// Targets must prove the receiver's representation supports their text reader before
    /// replacing the ordinary extension call with a runtime operation.
    TextTrim,
    /// Authenticated stdlib Iterable<T>.collectionSizeOrDefault(Int): Int. Native tests the
    /// Collection marker and invokes its selected size getter, never counting an Iterable.
    CollectionSizeOrDefault,
    /// The common stdlib `kotlin.ranges` `Double`/`Float` `rangeTo` and `rangeUntil` declarations.
    /// Membership in their closed or open-end result is an IEEE comparison. The provider copies
    /// this role from the exact common declaration onto its physical realization; a nearer
    /// operator and an otherwise identical declaration from another dependency do not carry it.
    FloatingRangeMembership,
    /// A `kotlin.ranges` `contains` extension of a closed range (`ClosedRange<Int>.contains(Long)`).
    /// kotlinc's `RangeContainsLowering` compares against the bounds of a range literal it is
    /// called on instead of calling it; a user extension of the same shape is called.
    ClosedRangeContains,
    /// The `kotlin.ranges` `rangeTo` extension of a `Comparable` value (kotlinc's
    /// `ComparableRangeToHandler`). Membership in the closed range it builds compares the value
    /// with both bounds through `Comparable.compareTo` instead of building the range.
    ComparableRangeTo,
    /// `kotlin.ranges` progression builders a counted `for` loop reads through instead of calling
    /// (kotlinc's `ForLoopsLowering` handlers). Outside a loop header they are ordinary calls.
    RangeDownTo,
    RangeUntil,
    ProgressionStep,
    ProgressionReversed,
    /// `withIndex()` over an array, an `Iterable`, a `CharSequence` or a `Sequence`. A `for` loop
    /// destructuring the `IndexedValue` it yields iterates the receiver and counts the index itself
    /// (kotlinc's `WithIndexHandler`). Anywhere else it is an ordinary call.
    WithIndex,
    /// The stdlib's unsigned comparison over the carrier of an unsigned value class
    /// (`uintCompare(Int, Int): Int`, `ulongCompare(Long, Long): Int`). kotlinc's counted loops
    /// call it to order unsigned bounds; a source call of it is an ordinary call.
    UnsignedCompare {
        carrier: crate::types::Ty,
    },
    /// The stdlib's unsigned division helper, at its declared unsigned operand type.
    UnsignedDivide {
        operand: crate::types::Ty,
    },
    /// The corresponding unsigned remainder helper.
    UnsignedRemainder {
        operand: crate::types::Ty,
    },
    /// A fully normalized `Result` helper reached from an expanded library inline body.
    ResultFailure(ResultFailureIntrinsic),
    /// `next()` declared by a Kotlin primitive iterator (`kotlin.collections.IntIterator.next`).
    /// A target may call the iterator's unboxed element operation instead of the boxed
    /// `Iterator<T>.next` bridge (kotlinc's `IteratorNext` intrinsic); outside that target rule the
    /// declaration is ordinary virtual dispatch.
    PrimitiveIteratorNext,
}
