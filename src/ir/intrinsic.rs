//! Compiler-supplied operations selected from semantic declarations.

use crate::types::Ty;

/// A compiler-supplied operation selected from a real semantic declaration. This is an operation
/// identity, not a library name: backends implement it without recovering signature facts from text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IrIntrinsic {
    /// Kotlin's checked `assert` operation. Arguments are the Boolean condition followed by its
    /// optional zero-argument message function. A backend must guard/elide the whole operation
    /// before evaluating either child according to `mode`.
    Assert {
        mode: crate::types::AssertionMode,
    },
    ArrayGet,
    ArraySet,
    ArraySize,
    StringGet,
    StringLength,
    /// `kotlin.Enum.name()` for a receiver that constant evaluation did not fold. A direct enum
    /// entry never reaches this operation: it is already the entry's name constant.
    EnumName,
    NullableAnyToString,
    /// Kotlin's compiler-supplied `enumValueOf<T>(name)`. `classifier` may remain a declaration-owned
    /// reified type parameter in the emitted inline template; call-site inline specialization turns
    /// it into the exact enum classifier without reopening resolution.
    EnumValueOf {
        classifier: Ty,
    },
    /// Kotlin's compiler-supplied zero-argument `enumEntries<T>()`. `classifier` may remain a
    /// declaration-owned reified type parameter in the emitted inline template; call-site inline
    /// specialization turns it into the exact enum classifier without reopening resolution.
    EnumEntries {
        classifier: Ty,
    },
    /// Kotlin's compiler-supplied `typeOf<T>()`. `ty` is the complete selected type argument,
    /// including nullability, arguments, and projections. It may name a declaration-owned reified
    /// type parameter in an emitted inline template, which the target realizes as its reified
    /// marker; any other type parameter it names is described to the runtime as a classifier.
    TypeOf {
        ty: Ty,
    },
    /// Result of an exact builtin scalar `compareTo` declaration. `operand` is the semantic common
    /// carrier selected by the frontend, not a JVM descriptor type. `relational_operator` records
    /// that this call came from FIR's `ComparisonCall`; an explicit `.compareTo()` remains false
    /// even when its integer result is later compared with zero.
    PrimitiveCompare {
        operand: Ty,
        relational_operator: bool,
    },
    /// Read the context from the current suspend continuation. The JVM coroutine pass replaces
    /// this operation with the continuation parameter's `Continuation.getContext()` call.
    CoroutineContext,
    /// One step of the coroutine protocol a target's shared state machine performs
    /// (`backend::coroutines::state_machine`). Produced only by that lowering, never by common
    /// lowering, so a target whose own coroutine pass runs instead never meets it.
    Coroutine(IrCoroutineOperation),
    UnsignedToString {
        source: Ty,
    },
    PrimitiveArrayNew {
        element: Ty,
    },
    /// Kotlin's IEEE-754 `==` between two operands of the floating type `operand`, at least one of
    /// them nullable: two nulls are equal, a null and a value are not, and two values compare as
    /// IEEE primitives. The arguments are the operands in source order, each with its own
    /// nullability; `!=` is the Boolean negation of this operation.
    Ieee754Equals {
        operand: Ty,
    },
    /// Generated-member equality of one property (a data class's, or a value class's sole one).
    /// Backends preserve Kotlin's scalar, floating-point, nullable, array-reference, and value-class
    /// equality semantics.
    GeneratedPropertyEquals {
        ty: Ty,
    },
    /// Generated-member hash of one property (a data class's, or a value class's sole one).
    GeneratedPropertyHash {
        ty: Ty,
    },
    /// Kotlin's content rendering for an array stored in a data-class property.
    DataClassArrayToString {
        ty: Ty,
    },
}

/// The coroutine-protocol operations a shared state machine needs from its target's runtime.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IrCoroutineOperation {
    /// The marker a suspend function returns when it suspended (`COROUTINE_SUSPENDED`). No
    /// arguments.
    Suspended,
    /// The raw resumption value carrying a failure (`Result.failure(exception)`). Argument: the
    /// exception.
    Failure,
    /// Throw the exception a raw resumption value carries, if it carries one. Argument: the value.
    ThrowOnFailure,
    /// `continuation.resumeWith(result)` on any continuation, the raw value standing for the
    /// `Result`. Arguments: the continuation and the raw value.
    ResumeWith,
    /// `continuation.context`. Argument: the continuation.
    ContextOf,
}
