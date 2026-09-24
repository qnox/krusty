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
    StringPlus,
    NullableAnyToString,
    /// Kotlin's compiler-supplied `enumValueOf<T>(name)`. `classifier` may remain a declaration-owned
    /// reified type parameter in the emitted inline template; call-site inline specialization turns
    /// it into the exact enum classifier without reopening resolution.
    EnumValueOf {
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
    UnsignedToString {
        source: Ty,
    },
    PrimitiveArrayNew {
        element: Ty,
    },
    /// Kotlin data-class equality for one primary-constructor property. Backends preserve Kotlin's
    /// scalar, floating-point, nullable, array-reference, and value-class equality semantics.
    DataClassFieldEquals {
        ty: Ty,
    },
    /// Kotlin data-class hash contribution for one primary-constructor property.
    DataClassFieldHash {
        ty: Ty,
    },
    /// Kotlin's content rendering for an array stored in a data-class property.
    DataClassArrayToString {
        ty: Ty,
    },
}
