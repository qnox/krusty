//! Provider-boundary realization of normalized Kotlin builtin members.
//!
//! `.kotlin_builtins` supplies the declaration identity, full source signature, and modifiers.
//! This module maps only an exact declaration to the compiler operation it denotes. The selected
//! backend decides how to implement that operation. A coincidental member spelling is never enough:
//! every operation below checks its semantic owner, complete parameter list, result, and the
//! modifier that makes the spelling an operator or infix declaration where Kotlin requires one.

use crate::libraries::{
    builtin_declaration::BuiltinMemberDeclaration, CompilerIntrinsic, MemberRealization,
    PrimitiveBinaryIntrinsic, PrimitiveUnaryIntrinsic,
};
use crate::types::{Ty, TypeName};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Scalar {
    Byte,
    Short,
    Int,
    Long,
    Float,
    Double,
    Char,
    Boolean,
}

impl Scalar {
    fn ty(self) -> Ty {
        match self {
            Self::Byte => Ty::Byte,
            Self::Short => Ty::Short,
            Self::Int => Ty::Int,
            Self::Long => Ty::Long,
            Self::Float => Ty::Float,
            Self::Double => Ty::Double,
            Self::Char => Ty::Char,
            Self::Boolean => Ty::Boolean,
        }
    }

    fn numeric_rank(self) -> Option<u8> {
        match self {
            Self::Byte | Self::Short | Self::Int => Some(0),
            Self::Long => Some(1),
            Self::Float => Some(2),
            Self::Double => Some(3),
            Self::Char | Self::Boolean => None,
        }
    }
}

fn owner_scalar(owner: TypeName) -> Option<Scalar> {
    if owner.matches("kotlin/Byte") {
        Some(Scalar::Byte)
    } else if owner.matches("kotlin/Short") {
        Some(Scalar::Short)
    } else if owner.matches("kotlin/Int") {
        Some(Scalar::Int)
    } else if owner.matches("kotlin/Long") {
        Some(Scalar::Long)
    } else if owner.matches("kotlin/Float") {
        Some(Scalar::Float)
    } else if owner.matches("kotlin/Double") {
        Some(Scalar::Double)
    } else if owner.matches("kotlin/Char") {
        Some(Scalar::Char)
    } else if owner.matches("kotlin/Boolean") {
        Some(Scalar::Boolean)
    } else {
        None
    }
}

/// Scalar types in a decoded builtin signature are already canonical `Ty` variants. Deliberately
/// reject nullable and object-shaped approximations instead of accepting a second representation.
fn signature_scalar(ty: Ty) -> Option<Scalar> {
    match ty {
        Ty::Byte => Some(Scalar::Byte),
        Ty::Short => Some(Scalar::Short),
        Ty::Int => Some(Scalar::Int),
        Ty::Long => Some(Scalar::Long),
        Ty::Float => Some(Scalar::Float),
        Ty::Double => Some(Scalar::Double),
        Ty::Char => Some(Scalar::Char),
        Ty::Boolean => Some(Scalar::Boolean),
        _ => None,
    }
}

fn promoted_numeric(left: Scalar, right: Scalar) -> Option<Ty> {
    let rank = left.numeric_rank()?.max(right.numeric_rank()?);
    Some(match rank {
        0 => Ty::Int,
        1 => Ty::Long,
        2 => Ty::Float,
        3 => Ty::Double,
        _ => return None,
    })
}

fn unary_result(receiver: Scalar) -> Option<Ty> {
    Some(match receiver {
        Scalar::Byte | Scalar::Short | Scalar::Int => Ty::Int,
        Scalar::Long => Ty::Long,
        Scalar::Float => Ty::Float,
        Scalar::Double => Ty::Double,
        Scalar::Char | Scalar::Boolean => return None,
    })
}

fn conversion_result(name: &str) -> Option<Ty> {
    Some(match name {
        "toByte" => Ty::Byte,
        "toShort" => Ty::Short,
        "toInt" => Ty::Int,
        "toLong" => Ty::Long,
        "toFloat" => Ty::Float,
        "toDouble" => Ty::Double,
        "toChar" => Ty::Char,
        _ => return None,
    })
}

fn primitive_binary(facts: &BuiltinMemberDeclaration<'_>) -> Option<CompilerIntrinsic> {
    if !facts.is_operator || facts.params.len() != 1 {
        return None;
    }
    let left = owner_scalar(facts.owner)?;
    let right = signature_scalar(facts.params[0])?;
    let expected = match (facts.name, left, right) {
        ("plus", Scalar::Char, Scalar::Int) => Ty::Char,
        ("minus", Scalar::Char, Scalar::Int) => Ty::Char,
        ("minus", Scalar::Char, Scalar::Char) => Ty::Int,
        ("plus" | "minus" | "times" | "div" | "rem", left, right) => promoted_numeric(left, right)?,
        _ => return None,
    };
    if facts.ret != expected {
        return None;
    }
    Some(CompilerIntrinsic::PrimitiveBinary(match facts.name {
        "plus" => PrimitiveBinaryIntrinsic::Add,
        "minus" => PrimitiveBinaryIntrinsic::Subtract,
        "times" => PrimitiveBinaryIntrinsic::Multiply,
        "div" => PrimitiveBinaryIntrinsic::Divide,
        "rem" => PrimitiveBinaryIntrinsic::Remainder,
        _ => return None,
    }))
}

fn primitive_compare(facts: &BuiltinMemberDeclaration<'_>) -> Option<CompilerIntrinsic> {
    if facts.name != "compareTo"
        || !facts.is_operator
        || facts.params.len() != 1
        || facts.ret != Ty::Int
    {
        return None;
    }
    let left = owner_scalar(facts.owner)?;
    let right = signature_scalar(facts.params[0])?;
    let valid = match (left, right) {
        (Scalar::Char, Scalar::Char) | (Scalar::Boolean, Scalar::Boolean) => true,
        (left, right) => left.numeric_rank().is_some() && right.numeric_rank().is_some(),
    };
    valid.then_some(CompilerIntrinsic::PrimitiveCompare)
}

fn primitive_bit_operation(facts: &BuiltinMemberDeclaration<'_>) -> Option<CompilerIntrinsic> {
    let receiver = owner_scalar(facts.owner)?;
    let receiver_ty = receiver.ty();
    match facts.name {
        "inv"
            if matches!(receiver, Scalar::Int | Scalar::Long)
                && facts.params.is_empty()
                && facts.ret == receiver_ty =>
        {
            Some(CompilerIntrinsic::PrimitiveBitNot)
        }
        "and" | "or" | "xor"
            if facts.is_infix
                && matches!(receiver, Scalar::Int | Scalar::Long | Scalar::Boolean)
                && facts.params == [receiver_ty]
                && facts.ret == receiver_ty =>
        {
            Some(match facts.name {
                "and" => CompilerIntrinsic::PrimitiveBitAnd,
                "or" => CompilerIntrinsic::PrimitiveBitOr,
                "xor" => CompilerIntrinsic::PrimitiveBitXor,
                _ => return None,
            })
        }
        "shl" | "shr" | "ushr"
            if facts.is_infix
                && matches!(receiver, Scalar::Int | Scalar::Long)
                && facts.params == [Ty::Int]
                && facts.ret == receiver_ty =>
        {
            Some(match facts.name {
                "shl" => CompilerIntrinsic::PrimitiveShiftLeft,
                "shr" => CompilerIntrinsic::PrimitiveShiftRight,
                "ushr" => CompilerIntrinsic::PrimitiveUnsignedShiftRight,
                _ => return None,
            })
        }
        _ => None,
    }
}

fn range_result(left: Scalar, right: Scalar) -> Option<Ty> {
    match (left, right) {
        (Scalar::Char, Scalar::Char) => Some(Ty::obj("kotlin/ranges/CharRange")),
        (left, right)
            if matches!(left, Scalar::Byte | Scalar::Short | Scalar::Int)
                && matches!(right, Scalar::Byte | Scalar::Short | Scalar::Int) =>
        {
            Some(Ty::obj("kotlin/ranges/IntRange"))
        }
        (left, right)
            if matches!(
                left,
                Scalar::Byte | Scalar::Short | Scalar::Int | Scalar::Long
            ) && matches!(
                right,
                Scalar::Byte | Scalar::Short | Scalar::Int | Scalar::Long
            ) =>
        {
            Some(Ty::obj("kotlin/ranges/LongRange"))
        }
        _ => None,
    }
}

fn range_construction(facts: &BuiltinMemberDeclaration<'_>) -> Option<MemberRealization> {
    let open_end = match facts.name {
        "rangeTo" => false,
        "rangeUntil" => true,
        _ => return None,
    };
    if !facts.is_operator || facts.params.len() != 1 {
        return None;
    }
    let result = range_result(
        owner_scalar(facts.owner)?,
        signature_scalar(facts.params[0])?,
    )?;
    (facts.ret == result).then_some(MemberRealization::RangeConstruction { open_end })
}

/// Semantic operand carriers `(receiver, argument)` of an exact primitive binary builtin. Arithmetic
/// and bitwise declarations expose the carrier/result type after Kotlin numeric promotion
/// (`Long.minus(Int)` returns `Long`, so both operands of the primitive subtraction are `Long`).
/// Shift declarations deliberately keep their count parameter distinct (`Long.shl(Int)`).
pub(crate) fn primitive_binary_operands(
    intrinsic: CompilerIntrinsic,
    parameter: Ty,
    result: Ty,
) -> Option<(Ty, Ty)> {
    let result = result.canonical_semantic().non_null();
    match intrinsic {
        CompilerIntrinsic::PrimitiveShiftLeft
        | CompilerIntrinsic::PrimitiveShiftRight
        | CompilerIntrinsic::PrimitiveUnsignedShiftRight => {
            Some((result, parameter.canonical_semantic().non_null()))
        }
        CompilerIntrinsic::PrimitiveBinary(_)
        | CompilerIntrinsic::PrimitiveBitAnd
        | CompilerIntrinsic::PrimitiveBitOr
        | CompilerIntrinsic::PrimitiveBitXor => Some((result, result)),
        _ => None,
    }
}

/// Common comparison carrier of an exact builtin scalar `compareTo`: `Char`/`Boolean` compare as
/// `Int`, numeric operands at their promoted type (`Int.compareTo(Long)` compares `Long`s).
pub(crate) fn primitive_compare_operand(receiver: Ty, parameter: Ty) -> Option<Ty> {
    let receiver = receiver.canonical_semantic().non_null();
    let parameter = parameter.canonical_semantic().non_null();
    if (receiver == Ty::Boolean && parameter == Ty::Boolean)
        || (receiver == Ty::Char && parameter == Ty::Char)
    {
        Some(Ty::Int)
    } else {
        Ty::promote(receiver, parameter)
    }
}

/// Attach a compiler realization only to an exact normalized builtin declaration.
pub(crate) fn realization(facts: BuiltinMemberDeclaration<'_>) -> MemberRealization {
    if facts.is_property {
        return MemberRealization::Dispatch;
    }

    if let Some(intrinsic) = primitive_binary(&facts) {
        return MemberRealization::Intrinsic(intrinsic);
    }
    if let Some(intrinsic) = primitive_compare(&facts) {
        return MemberRealization::Intrinsic(intrinsic);
    }
    if let Some(intrinsic) = primitive_bit_operation(&facts) {
        return MemberRealization::Intrinsic(intrinsic);
    }

    let receiver = owner_scalar(facts.owner);
    if matches!(facts.name, "unaryPlus" | "unaryMinus")
        && facts.is_operator
        && facts.params.is_empty()
        && receiver.and_then(unary_result) == Some(facts.ret)
    {
        return MemberRealization::Intrinsic(CompilerIntrinsic::PrimitiveUnary(
            if facts.name == "unaryPlus" {
                PrimitiveUnaryIntrinsic::Identity
            } else {
                PrimitiveUnaryIntrinsic::Negate
            },
        ));
    }

    if facts.params.is_empty()
        && receiver.is_some_and(|scalar| scalar != Scalar::Boolean)
        && conversion_result(facts.name) == Some(facts.ret)
    {
        return MemberRealization::Intrinsic(CompilerIntrinsic::NumericConversion);
    }

    if facts.owner.matches("kotlin/Boolean")
        && facts.name == "not"
        && facts.is_operator
        && facts.params.is_empty()
        && facts.ret == Ty::Boolean
    {
        return MemberRealization::Intrinsic(CompilerIntrinsic::BooleanNot);
    }

    if facts.owner.matches("kotlin/String")
        && facts.name == "plus"
        && facts.is_operator
        && facts.params == [Ty::nullable(Ty::obj("kotlin/Any"))]
        && facts.ret == Ty::String
    {
        return MemberRealization::Intrinsic(CompilerIntrinsic::StringPlus);
    }

    match range_construction(&facts) {
        Some(realization) => realization,
        None => MemberRealization::Dispatch,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::type_name;

    fn facts<'a>(
        owner: &'a str,
        name: &'a str,
        params: &'a [Ty],
        ret: Ty,
    ) -> BuiltinMemberDeclaration<'a> {
        BuiltinMemberDeclaration {
            owner: type_name(owner),
            name,
            params,
            ret,
            is_property: false,
            is_operator: true,
            is_infix: false,
        }
    }

    #[test]
    fn scalar_operation_requires_the_complete_normalized_signature() {
        assert_eq!(
            realization(facts("kotlin/Int", "plus", &[Ty::Long], Ty::Long)),
            MemberRealization::Intrinsic(CompilerIntrinsic::PrimitiveBinary(
                PrimitiveBinaryIntrinsic::Add
            ))
        );
        assert_eq!(
            realization(facts("sample/Counter", "plus", &[Ty::Int], Ty::Int)),
            MemberRealization::Dispatch
        );
        assert_eq!(
            realization(facts(
                "kotlin/Int",
                "plus",
                &[Ty::obj("sample/Counter")],
                Ty::Int,
            )),
            MemberRealization::Dispatch
        );
        assert_eq!(
            realization(facts(
                "kotlin/Int",
                "plus",
                &[Ty::nullable(Ty::Int)],
                Ty::Int,
            )),
            MemberRealization::Dispatch
        );
        assert_eq!(
            realization(facts("kotlin/Int", "plus", &[Ty::Long], Ty::Int)),
            MemberRealization::Dispatch
        );
    }

    #[test]
    fn range_construction_requires_a_builtin_range_signature() {
        assert_eq!(
            realization(facts(
                "kotlin/Short",
                "rangeTo",
                &[Ty::Byte],
                Ty::obj("kotlin/ranges/IntRange"),
            )),
            MemberRealization::RangeConstruction { open_end: false }
        );
        assert_eq!(
            realization(facts(
                "sample/Bound",
                "rangeTo",
                &[Ty::obj("sample/Bound")],
                Ty::obj("sample/Span"),
            )),
            MemberRealization::Dispatch
        );
        assert_eq!(
            realization(facts(
                "kotlin/Int",
                "rangeTo",
                &[Ty::Int],
                Ty::obj("sample/Span"),
            )),
            MemberRealization::Dispatch
        );
    }
}
