//! Compiler-supplied realization identities attached to selected declarations.

use super::{PrimitiveBinaryIntrinsic, PrimitiveUnaryIntrinsic};

/// The exact library helper used to construct or inspect a failed `Result`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResultFailureIntrinsic {
    Create,
    Check,
    CheckOfClass,
}

/// Which name of a class token an authenticated `kotlin.reflect.KClass` getter reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClassNameKind {
    /// `simpleName`.
    Simple,
    /// `qualifiedName`.
    Qualified,
}

/// Which characters of a `CharArray` an authenticated conversion to a `String` reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CharArrayRange {
    /// The whole array: `concatToString()`.
    Whole,
    /// `concatToString(startIndex, endIndex)`, the characters from `startIndex` until `endIndex`.
    StartEnd,
}

/// Which neighbour of a floating-point value an authenticated `nextUp`/`nextDown` answers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AdjacentDirection {
    /// `nextUp()`, the adjacent representable value toward positive infinity.
    Up,
    /// `nextDown()`, the adjacent representable value toward negative infinity.
    Down,
}

/// Which half of a UTF-16 surrogate pair an authenticated `Char` predicate recognizes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SurrogateHalf {
    /// `Char.isHighSurrogate()`, the leading half `\uD800..=\uDBFF`.
    High,
    /// `Char.isLowSurrogate()`, the trailing half `\uDC00..=\uDFFF`.
    Low,
}

impl SurrogateHalf {
    /// The inclusive UTF-16 code-unit range of this half.
    pub fn code_units(self) -> std::ops::RangeInclusive<u16> {
        match self {
            Self::High => 0xD800..=0xDBFF,
            Self::Low => 0xDC00..=0xDFFF,
        }
    }
}

/// Storage supplied by a target for an authenticated stdlib collection factory. The selected
/// declaration distinguishes one array element from a packed array of elements before substitution.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ListStorageConstruction {
    ReadonlyEmpty,
    ReadonlySingleton,
    ReadonlyPacked,
    MutableEmpty,
    MutablePacked,
}

/// One access a target realizes as a single atomic instruction on one storage location. The
/// operand list after the location follows the declaration: a stored value, an expected value and
/// its replacement, or an addend.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum AtomicOperation {
    /// Read the value.
    Load,
    /// Write `newValue`.
    Store,
    /// Write `newValue` and answer the value it replaced.
    Exchange,
    /// Write `newValue` when the value is `expectedValue`, and answer whether it did.
    CompareAndSet,
    /// Write `newValue` when the value is `expectedValue`, and answer the value it found.
    CompareAndExchange,
    /// Add `delta` and answer the value before the addition.
    FetchAndAdd,
}

impl AtomicOperation {
    /// The operands after the storage location.
    pub const fn operands(self) -> usize {
        match self {
            Self::Load => 0,
            Self::Store | Self::Exchange | Self::FetchAndAdd => 1,
            Self::CompareAndSet | Self::CompareAndExchange => 2,
        }
    }
}

/// A source-declared callable whose implementation is supplied by the compiler after ordinary symbol
/// and overload selection. Providers attach this to the exact declaration identity; lowering never
/// grants intrinsic behavior from a coincidental source name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CompilerIntrinsic {
    /// An authenticated platform-stdlib exception constructor with no arguments or one `String?`.
    /// The target owns storage for this exact declaring kind, including the nullable message.
    ThrowableConstructor {
        kind: super::RuntimeExceptionKind,
        message: bool,
    },
    /// The authenticated `Throwable.message: String?` getter, not a same-named user property.
    ThrowableMessage,
    /// The authenticated `Throwable.cause: Throwable?` getter, not a same-named user property.
    ThrowableCause,
    /// The authenticated platform-stdlib `KClass.simpleName` or `qualifiedName` getter. A
    /// target that builds its own class tokens answers it from the token's reflection names.
    ClassName(ClassNameKind),
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
    /// Native's exact `GCUnsafeCall` `nextUp`/`nextDown` declaration: the adjacent representable
    /// value of the extension receiver, at the receiver's own width.
    FloatingAdjacent {
        operand: crate::types::Ty,
        direction: AdjacentDirection,
    },
    /// Native's exact `GCUnsafeCall` `countLeadingZeroBits` declaration of `Int` or `Long`: the
    /// zero bits above the highest set one, the whole width for zero.
    CountLeadingZeroBits {
        operand: crate::types::Ty,
    },
    /// Native's exact `GCUnsafeCall` `Char` surrogate predicate declaration. A target answers it
    /// with a range test of the receiver's code unit.
    CharIsSurrogate(SurrogateHalf),
    /// Exact Kotlin array-factory declaration whose implementation is supplied by the compiler.
    /// The kind distinguishes reference/primitive varargs and the other language array creators;
    /// selected call-site types remain ordinary semantic types and are recorded in checked FIR.
    ArrayFactory(crate::types::ArrayFactoryKind),
    /// A target may supply its collection storage instead of calling the declaration's body.
    /// This is not common lowering of a library algorithm; other targets keep the ordinary call.
    ListStorageConstruction(ListStorageConstruction),
    /// An array classifier's own `operator fun get(index: Int)` (`IntArray.get`, `Array<T>.get`).
    /// The array classifiers have no platform class, so a target reads the element directly.
    ArrayGet,
    /// An array classifier's own `operator fun set(index: Int, value)`, realized as a direct
    /// element store for the same reason as [`Self::ArrayGet`].
    ArraySet,
    ArraySize,
    /// The Wasm stdlib's `wasm_array_copy<T>(destination, destinationIndex, source, sourceIndex,
    /// length)`, an `@ExcludedFromCodegen` declaration kotlinc lowers to `array.copy` over the
    /// two storage arrays.
    ArrayStorageCopy,
    CharCode,
    StringLength,
    /// `String.get(Int): Char`, the index operator on a string.
    StringGet,
    StringPlus,
    /// `String.compareTo(String): Int`, the lexicographic order of two strings by UTF-16 code
    /// units. A target that lays out strings itself orders them itself, as it reads their length
    /// and code units.
    StringCompare,
    /// `String.subSequence(Int, Int): CharSequence`, the characters between two indices of a
    /// string, which is itself a string. A target that lays out strings itself copies them.
    StringSubSequence,
    /// `CharArray.concatToString` of `kotlin.text`: the string of a range of a char array's units. A target that lays out strings itself checks the
    /// range as the stdlib does and copies the units.
    CharArrayToString(CharArrayRange),
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
    /// Kotlin's `assert`. `platform_default` is how the platform compiles it when no
    /// `-Xassertions` mode is selected: kotlinc's JVM compiler defaults to
    /// [`AssertionCompilation::Legacy`](crate::types::AssertionCompilation::Legacy), an ordinary
    /// inline call.
    Assert {
        platform_default: crate::types::AssertionCompilation,
    },
    AssertFailsWith,
    /// The Native/Wasm stdlib's `isAssertionThrowingErrorEnabled()` and
    /// `isAssertionArgumentEvaluationEnabled()`, which the compiler answers for the whole build.
    /// Both answer the assertion switch of kotlinc's default build, which is off.
    AssertionSwitch,
    Print,
    Println,
    StartCoroutine,
    /// The current suspend body's continuation context. The stdlib declaration is a public
    /// `@InlineOnly` suspend property whose private throwing accessor is never invoked directly.
    CoroutineContext,
    CoroutineSuspended,
    SuspendCoroutineUninterceptedOrReturn,
    /// A target stdlib's `startCoroutineUninterceptedOrReturnIntrinsicN(f, inputs.., completion)`:
    /// the suspend function value `f` invoked with its inputs and `completion` as its
    /// continuation. Its stdlib body only says the compiler implements it.
    StartCoroutineUninterceptedOrReturn,
    /// Native's `kotlin.native.internal.getContinuation<T>()`, an external typed intrinsic: the
    /// enclosing suspend body's own continuation. A KLIB body reaches it where kotlinc's
    /// pre-serialization inliner expanded `suspendCoroutineUninterceptedOrReturn`.
    CurrentContinuation,
    /// Native's `kotlin.native.internal.returnIfSuspended<T>(argument)`, an external suspend typed
    /// intrinsic: the enclosing suspend body suspends when `argument` is `COROUTINE_SUSPENDED`
    /// and otherwise goes on with `argument` as its `T`. It is the suspension point of an expanded
    /// `suspendCoroutineUninterceptedOrReturn`, whose block computed `argument`.
    ReturnIfSuspended,
    /// Kotlin/Wasm's `kotlin.wasm.internal.interceptedIntrinsic<T>(cont)`, an
    /// `@ExcludedFromCodegen` declaration behind `Continuation<T>.intercepted()`. kotlinc's
    /// `BuiltInsLowering` expands it to `if (cont is CoroutineImpl) cont.intercepted() else cont`
    /// over the stdlib's state-machine `kotlin.coroutines.CoroutineImpl`.
    InterceptedContinuation,
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
    /// The `indices` extension property of an array, a `Collection` or a `CharSequence`. A `for`
    /// loop over it counts from 0 to the receiver's size instead of building the `IntRange`
    /// (kotlinc's `IndicesHandler`). Anywhere else it is an ordinary read.
    Indices(IndicesReceiver),
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
    /// Native's runtime rendering of an `Int` or `Long` in a radix the stdlib already checked
    /// (`kotlin.text.intToString`/`longToString`, `@GCUnsafeCall` externals without a body).
    RadixToString {
        operand: crate::types::Ty,
    },
    /// Native's runtime comparison of two `String` ranges' UTF-16 units, whose bounds the stdlib
    /// already checked (`kotlin.text.unsafeRangeEquals`, a private `@GCUnsafeCall` external).
    StringRangeEquals,
    /// Native's runtime search of a `String` for a `String` or a `Char` from a start index
    /// (`kotlin.text.nativeIndexOf` and `nativeLastIndexOf`, `@GCUnsafeCall` externals), forward
    /// or, for `last`, backward. The sought value is the second operand, of type `sought`.
    StringIndexOf {
        sought: crate::types::Ty,
        last: bool,
    },
    /// Native's runtime copy of `count` elements of one array into another of its kind, a
    /// specialized primitive array or `Array<Any?>` (`kotlin.collections.arrayCopy`, a
    /// `@GCUnsafeCall` external without a body). It checks both ranges itself and copies as if
    /// through a temporary buffer, so the two ranges may overlap within one array.
    ArrayCopy {
        array: crate::types::Ty,
    },
    /// A fully normalized `Result` helper reached from an expanded library inline body.
    ResultFailure(ResultFailureIntrinsic),
    /// The `exception` property getter on the compiler-builtin `Result.Failure` carrier. The
    /// target owns that carrier's storage and reads it through its runtime ABI.
    ResultFailureException,
    /// `next()` declared by a Kotlin primitive iterator (`kotlin.collections.IntIterator.next`).
    /// A target may call the iterator's unboxed element operation instead of the boxed
    /// `Iterator<T>.next` bridge (kotlinc's `IteratorNext` intrinsic); outside that target rule the
    /// declaration is ordinary virtual dispatch.
    PrimitiveIteratorNext,
}

impl CompilerIntrinsic {
    /// Whether the declaration's own body says nothing of what a call does: it only throws, and
    /// the compiler supplies the whole behavior from the selected type argument. Such a call is
    /// never expanded from that body, even where the library serializes it as an inline template.
    pub fn replaces_its_body(self) -> bool {
        matches!(self, Self::TypeOf | Self::EnumEntries)
    }
}

/// The receiver class of an `indices` property, which names the member kotlinc's `IndicesHandler`
/// bounds the loop with: an array's or a `Collection`'s `size`, a `CharSequence`'s `length`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IndicesReceiver {
    Array,
    Collection,
    CharSequence,
}

impl IndicesReceiver {
    /// The property `IndicesHandler` bounds a loop over the indices of this receiver kind with.
    pub fn bound_property(self) -> &'static str {
        match self {
            Self::Array | Self::Collection => crate::types::wk::SIZE,
            Self::CharSequence => crate::types::wk::CHAR_SEQUENCE_LENGTH,
        }
    }

    /// The one class declaring that property for every receiver of this kind. An array kind
    /// spans many array classes, so none names it.
    pub fn bound_class(self) -> Option<crate::types::TypeName> {
        match self {
            Self::Array => None,
            Self::Collection => Some(crate::types::wk::collection()),
            Self::CharSequence => Some(crate::types::wk::char_sequence()),
        }
    }
}
