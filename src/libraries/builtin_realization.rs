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
    CompilerIntrinsic, MemberRealization, PrimitiveBinaryIntrinsic, PrimitiveUnaryIntrinsic,
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
