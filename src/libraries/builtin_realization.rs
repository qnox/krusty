//! What a scalar builtin member IS, as opposed to what it is declared as.
//!
//! Kotlin declares `Int.plus(Int): Int`, `Double.compareTo(Double): Int` and `Int.toByte(): Byte`
//! as ordinary members, because at the source level that is exactly what they are: they are
//! resolved, overloaded and imported like any other. No target implements them by dispatching to a
//! method. The JVM emits `iadd`; the native code generator emits a machine instruction; neither has
//! a callee to call.
//!
//! Which declaration is which is a fact about the SIGNATURE — an owner that is a scalar, a name,
//! an arity, a result that is a scalar — and not about any artifact it was read from. It was
//! nevertheless written inside the JVM classpath's builtins decoder, where the `.kotlin_builtins`
//! file happened to be parsed, so a second provider reading the same declarations out of a KLIB
//! published them as real member calls and the code generator declined them: `kotlin.Int.plus`
//! alone, 138 times across the box corpus.
//!
//! This module is that rule, stated once, over the signature every provider already has.

use crate::libraries::{
    CompilerIntrinsic, FnKind, MemberRealization, PrimitiveBinaryIntrinsic, PrimitiveUnaryIntrinsic,
};
use crate::types::{Ty, TypeName};

/// The Kotlin scalar a classifier identity names, or `None` when it names anything else.
fn scalar(name: TypeName) -> Option<Ty> {
    [
        ("kotlin/Int", Ty::Int),
        ("kotlin/Byte", Ty::Byte),
        ("kotlin/Short", Ty::Short),
        ("kotlin/Long", Ty::Long),
        ("kotlin/Float", Ty::Float),
        ("kotlin/Double", Ty::Double),
        ("kotlin/Char", Ty::Char),
        ("kotlin/Boolean", Ty::Boolean),
    ]
    .into_iter()
    .find_map(|(candidate, ty)| name.matches(candidate).then_some(ty))
}

/// The scalar a declared result denotes. A provider may spell a scalar result either as the
/// classifier (`kotlin/Int`) or already as the carrier, so both are admitted.
fn result_scalar(ret: Ty) -> Option<Ty> {
    match ret.non_null() {
        Ty::Obj(name, _) => scalar(name),
        result => result.scalar_value_repr(),
    }
}

/// How a member with this exact signature is realized: the operation it denotes, or ordinary
/// dispatch when it denotes none.
///
/// `params` and `ret` are the DECLARED semantic signature — the member's own, with no receiver
/// prepended and no erasure applied.
pub(crate) fn member_realization(
    owner: TypeName,
    name: &str,
    params: &[Ty],
    ret: Ty,
) -> MemberRealization {
    let owner_scalar = scalar(owner);
    let declared_result = result_scalar(ret);
    let intrinsic = match (owner_scalar, name, params.len(), declared_result) {
        // `+x` / `-x`. The result names the PROMOTED carrier (`Byte.unaryMinus(): Int`), which is
        // why the declaration is consulted rather than the receiver.
        (Some(_), "unaryPlus", 0, Some(_)) => {
            CompilerIntrinsic::PrimitiveUnary(PrimitiveUnaryIntrinsic::Identity)
        }
        (Some(_), "unaryMinus", 0, Some(_)) => {
            CompilerIntrinsic::PrimitiveUnary(PrimitiveUnaryIntrinsic::Negate)
        }
        (
            Some(_),
            "toInt" | "toByte" | "toShort" | "toLong" | "toFloat" | "toDouble" | "toChar",
            0,
            Some(_),
        ) => CompilerIntrinsic::NumericConversion,
        (Some(_), "plus" | "minus" | "times" | "div" | "rem", 1, Some(_)) => {
            CompilerIntrinsic::PrimitiveBinary(match name {
                "plus" => PrimitiveBinaryIntrinsic::Add,
                "minus" => PrimitiveBinaryIntrinsic::Subtract,
                "times" => PrimitiveBinaryIntrinsic::Multiply,
                "div" => PrimitiveBinaryIntrinsic::Divide,
                "rem" => PrimitiveBinaryIntrinsic::Remainder,
                _ => unreachable!("the arm admits only primitive binary arithmetic"),
            })
        }
        // A scalar `compareTo` is the comparison only when its OPERAND is a scalar too:
        // `Int.compareTo(Any?)` is not this declaration, and `Comparable.compareTo` is dispatch.
        (Some(_), "compareTo", 1, Some(Ty::Int))
            if match params[0].non_null() {
                Ty::Obj(parameter, _) => scalar(parameter).is_some(),
                parameter => parameter.is_jvm_scalar(),
            } =>
        {
            CompilerIntrinsic::PrimitiveCompare
        }
        (Some(Ty::Boolean), "not", 0, Some(Ty::Boolean)) => CompilerIntrinsic::BooleanNot,
        (Some(receiver @ (Ty::Int | Ty::Long | Ty::Boolean)), "inv", 0, _) if ret == receiver => {
            CompilerIntrinsic::PrimitiveBitNot
        }
        (Some(receiver @ (Ty::Int | Ty::Long | Ty::Boolean)), "and" | "or" | "xor", 1, _)
            if ret == receiver && params == [receiver] =>
        {
            match name {
                "and" => CompilerIntrinsic::PrimitiveBitAnd,
                "or" => CompilerIntrinsic::PrimitiveBitOr,
                "xor" => CompilerIntrinsic::PrimitiveBitXor,
                _ => unreachable!("the arm admits only the three bitwise operations"),
            }
        }
        // A shift counts in `Int` whatever it shifts, and `Boolean` has none.
        (Some(receiver @ (Ty::Int | Ty::Long)), "shl" | "shr" | "ushr", 1, _)
            if ret == receiver && params == [Ty::Int] =>
        {
            match name {
                "shl" => CompilerIntrinsic::PrimitiveShiftLeft,
                "shr" => CompilerIntrinsic::PrimitiveShiftRight,
                "ushr" => CompilerIntrinsic::PrimitiveUnsignedShiftRight,
                _ => unreachable!("the arm admits only the three shifts"),
            }
        }
        _ if owner.matches("kotlin/String")
            && name == "plus"
            && params == [Ty::nullable(Ty::obj("kotlin/Any"))]
            && ret == Ty::String =>
        {
            CompilerIntrinsic::StringPlus
        }
        // A range is CONSTRUCTED rather than called, on every receiver — `1..10`, `'a'..'z'`,
        // `Instant.DISTANT_PAST..now`. The rule is the name, not the operand type.
        _ if name == "rangeTo" => return MemberRealization::RangeConstruction { open_end: false },
        _ if name == "rangeUntil" => {
            return MemberRealization::RangeConstruction { open_end: true }
        }
        _ => return MemberRealization::Dispatch,
    };
    MemberRealization::Intrinsic(intrinsic)
}

/// Which compiler intrinsic a TOP-LEVEL declaration of this package and name is, or `None` for an
/// ordinary one.
///
/// Like [`member_realization`], a fact about what Kotlin DECLARES and not about any artifact:
/// `kotlin.io.println` is realized by the compiler on every target, and `String.trimIndent()` is
/// folded rather than called on every target. It lived in the JVM provider because that was the
/// only provider, so a klib-backed compilation declined `trimIndent` 57 times over the box corpus
/// while the jar-backed one folded it.
pub(crate) fn top_level_intrinsic(
    package: TypeName,
    name: &str,
    receiver: Option<Ty>,
) -> Option<CompilerIntrinsic> {
    if package.matches("kotlin/coroutines") {
        return match name {
            "suspendCoroutine" => Some(CompilerIntrinsic::SuspendCoroutine),
            "startCoroutine" => Some(CompilerIntrinsic::StartCoroutine),
            _ => None,
        };
    }
    if package.matches("kotlin/coroutines/intrinsics") {
        return (name == "suspendCoroutineUninterceptedOrReturn")
            .then_some(CompilerIntrinsic::SuspendCoroutineUninterceptedOrReturn);
    }
    if package.matches("kotlin/io") {
        return match name {
            "print" => Some(CompilerIntrinsic::Print),
            "println" => Some(CompilerIntrinsic::Println),
            _ => None,
        };
    }
    if package.matches("kotlin") {
        return match name {
            "assert" => Some(CompilerIntrinsic::Assert),
            "enumValues" => Some(CompilerIntrinsic::EnumValues),
            "enumValueOf" => Some(CompilerIntrinsic::EnumValueOf),
            // String concatenation, which Kotlin declares as `String?.plus(Any?)`. The receiver is
            // checked because `plus` is a name any package may declare on anything, and a target
            // realizes THIS one as a concatenation rather than a call.
            "plus" if receiver.map(Ty::non_null) == Some(Ty::String) => {
                Some(CompilerIntrinsic::StringPlus)
            }
            _ => crate::libraries::kotlin_array_factory_kind(name)
                .map(CompilerIntrinsic::ArrayFactory),
        };
    }
    if package.matches("kotlin/test") {
        return (name == "assertFailsWith").then_some(CompilerIntrinsic::AssertFailsWith);
    }
    if package.matches("kotlin/collections") {
        return match name {
            "isEmpty" => Some(CompilerIntrinsic::IsEmpty),
            "isNotEmpty" => Some(CompilerIntrinsic::IsNotEmpty),
            "count" => Some(CompilerIntrinsic::Count),
            "trimIndent" => Some(CompilerIntrinsic::TrimIndent),
            "trimMargin" => Some(CompilerIntrinsic::TrimMargin),
            _ => None,
        };
    }
    if package.matches("kotlin/text") {
        return match name {
            "trimIndent" => Some(CompilerIntrinsic::TrimIndent),
            "trimMargin" => Some(CompilerIntrinsic::TrimMargin),
            _ => None,
        };
    }
    None
}

/// The declaration KIND [`top_level_intrinsic`]'s answer applies to.
///
/// A name can be declared twice — `kotlin.collections.count()` is an extension on a collection and
/// `kotlin.text.count()` one on a `CharSequence`, while `println` is receiverless — so the
/// intrinsic is stamped only on the overload that has the right shape. `None` marks an intrinsic
/// that is never a top-level declaration at all: a scalar member's, which
/// [`member_realization`] answers for instead.
pub(crate) fn intrinsic_declaration_kind(intrinsic: CompilerIntrinsic) -> Option<FnKind> {
    Some(match intrinsic {
        CompilerIntrinsic::ArrayFactory(_)
        | CompilerIntrinsic::Print
        | CompilerIntrinsic::Println
        | CompilerIntrinsic::Assert
        | CompilerIntrinsic::AssertFailsWith
        | CompilerIntrinsic::CoroutineContext
        | CompilerIntrinsic::CoroutineSuspended
        | CompilerIntrinsic::SuspendCoroutine
        | CompilerIntrinsic::SuspendCoroutineUninterceptedOrReturn
        | CompilerIntrinsic::EnumValues
        | CompilerIntrinsic::EnumValueOf => FnKind::TopLevel,
        CompilerIntrinsic::StartCoroutine
        | CompilerIntrinsic::IsEmpty
        | CompilerIntrinsic::IsNotEmpty
        | CompilerIntrinsic::Count
        | CompilerIntrinsic::TrimIndent
        | CompilerIntrinsic::TrimMargin
        | CompilerIntrinsic::StringPlus
        | CompilerIntrinsic::NullableAnyToString => FnKind::Extension,
        CompilerIntrinsic::ArraySize
        | CompilerIntrinsic::CharCode
        | CompilerIntrinsic::StringLength
        | CompilerIntrinsic::NumericConversion
        | CompilerIntrinsic::PrimitiveUnary(_)
        | CompilerIntrinsic::PrimitiveCompare
        | CompilerIntrinsic::PrimitiveBitAnd
        | CompilerIntrinsic::PrimitiveBitOr
        | CompilerIntrinsic::PrimitiveBitXor
        | CompilerIntrinsic::PrimitiveShiftLeft
        | CompilerIntrinsic::PrimitiveShiftRight
        | CompilerIntrinsic::PrimitiveUnsignedShiftRight
        | CompilerIntrinsic::BooleanNot
        | CompilerIntrinsic::PrimitiveBitNot
        | CompilerIntrinsic::PrimitiveBinary(_) => return None,
    })
}

/// Which compiler intrinsic a top-level or extension PROPERTY of this package and name is.
///
/// Both of them are extensions and both are told by their receiver as well as their name:
/// `kotlin.code` is a property of `Char` and of nothing else.
pub(crate) fn top_level_property_intrinsic(
    package: TypeName,
    name: &str,
    receiver: Option<Ty>,
) -> Option<CompilerIntrinsic> {
    // Receiverless: the suspension sentinel is a top-level property, and an extension of the same
    // name would be a different declaration.
    if package.matches("kotlin/coroutines/intrinsics")
        && name == "COROUTINE_SUSPENDED"
        && receiver.is_none()
    {
        return Some(CompilerIntrinsic::CoroutineSuspended);
    }
    if package.matches("kotlin") && name == "code" && receiver == Some(Ty::Char) {
        return Some(CompilerIntrinsic::CharCode);
    }
    None
}

/// The reflection classifier a PROPERTY reference has, given its arity and mutability.
///
/// `A::x` is a `kotlin.reflect.KProperty1<A, Int>`. The name is Kotlin's own on every target —
/// nothing about `KProperty1` is the JVM's — so this is stated once rather than per provider. A
/// provider that did not answer gave a property reference no type at all, and the reference read
/// as an unresolved name: `A::x` reported "unresolved reference 'x'".
///
/// `args` are the classifier's own type arguments in declaration order: `[V]` for a `KProperty0`,
/// `[Recv, V]` for a `KProperty1`. They are required, because a RAW `KProperty0` exposes the
/// declaration's unbound `V` to the checker instead of the property's own type.
pub(crate) fn property_reference_classifier(
    arity: usize,
    mutable: bool,
    args: &[Ty],
) -> Option<Ty> {
    let internal = match (arity, mutable) {
        (0, false) => "kotlin/reflect/KProperty0",
        (0, true) => "kotlin/reflect/KMutableProperty0",
        (1, false) => "kotlin/reflect/KProperty1",
        (1, true) => "kotlin/reflect/KMutableProperty1",
        _ => return None,
    };
    if args.len() != arity + 1 || args.contains(&Ty::Error) {
        return None;
    }
    Some(Ty::obj_args(internal, args))
}

/// The reflection classifier a FUNCTION reference has, from its already-resolved signature.
///
/// The direction matters and is the same one [`property_reference_classifier`] takes: the
/// signature selects the classifier, and no consumer parses a classifier name to reconstruct a
/// signature.
pub(crate) fn function_reference_classifier(function: Ty) -> Option<Ty> {
    let Ty::Fun(signature) = function else {
        return None;
    };
    if signature.context_count != 0 {
        return None;
    }
    // An extension receiver is already the first semantic parameter of `FnSig`; `has_receiver`
    // describes invocation syntax, not a different reflective arity. So `Int::extension` has the
    // ordinary reflection type `KFunction1<Int, R>`.
    let mut arguments = signature.params.to_vec();
    arguments.push(signature.ret);
    let classifier = crate::types::type_name_child(
        crate::types::type_name("kotlin/reflect"),
        &format!(
            "{}{}",
            if signature.suspend {
                "KSuspendFunction"
            } else {
                "KFunction"
            },
            signature.params.len()
        ),
    );
    Some(Ty::obj_args_name(classifier, &arguments))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::type_name;

    /// The signature decides, not the name alone: the same spelling on a non-scalar owner, or with
    /// a non-scalar operand, is an ordinary call.
    #[test]
    fn a_scalar_operation_is_recognized_by_its_whole_signature() {
        assert_eq!(
            member_realization(type_name("kotlin/Int"), "plus", &[Ty::Int], Ty::Int),
            MemberRealization::Intrinsic(CompilerIntrinsic::PrimitiveBinary(
                PrimitiveBinaryIntrinsic::Add
            ))
        );
        // A declaration spelling its result as the CLASSIFIER, which is how a klib writes it.
        assert_eq!(
            member_realization(
                type_name("kotlin/Int"),
                "plus",
                &[Ty::obj("kotlin/Long")],
                Ty::obj("kotlin/Long"),
            ),
            MemberRealization::Intrinsic(CompilerIntrinsic::PrimitiveBinary(
                PrimitiveBinaryIntrinsic::Add
            ))
        );
        // `BigInteger.plus` is a real call.
        assert_eq!(
            member_realization(
                type_name("java/math/BigInteger"),
                "plus",
                &[Ty::obj("java/math/BigInteger")],
                Ty::obj("java/math/BigInteger"),
            ),
            MemberRealization::Dispatch
        );
        // `Int.compareTo(Any?)` does not exist, but a provider that published it must not have it
        // read as the primitive comparison.
        assert_eq!(
            member_realization(
                type_name("kotlin/Int"),
                "compareTo",
                &[Ty::nullable(Ty::obj("kotlin/Any"))],
                Ty::Int,
            ),
            MemberRealization::Dispatch
        );
        // And `Boolean` has no shift, however its operands are spelled.
        assert_eq!(
            member_realization(type_name("kotlin/Boolean"), "shl", &[Ty::Int], Ty::Boolean,),
            MemberRealization::Dispatch
        );
    }

    #[test]
    fn a_range_is_constructed_on_any_receiver() {
        assert_eq!(
            member_realization(
                type_name("kotlin/Int"),
                "rangeTo",
                &[Ty::Int],
                Ty::obj("kotlin/ranges/IntRange")
            ),
            MemberRealization::RangeConstruction { open_end: false }
        );
        assert_eq!(
            member_realization(
                type_name("kotlin/time/Instant"),
                "rangeUntil",
                &[Ty::obj("kotlin/time/Instant")],
                Ty::obj("kotlin/ranges/ClosedRange"),
            ),
            MemberRealization::RangeConstruction { open_end: true }
        );
    }
}
