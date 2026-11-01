//! Provider-boundary classification of the stdlib numeric declarations kotlinc evaluates at
//! compile time.
//!
//! kotlinc's `ConstEvaluationLowering` folds a call to any declaration marked
//! `@IntrinsicConstEvaluation` once its operands are constants. Most of those declarations are
//! already compiler intrinsics or carry an override role (`toString`, `equals`, `compareTo`). The
//! unsigned value classes' operations and conversions, the signed-to-unsigned conversions, and
//! `floorDiv`/`mod` are ordinary stdlib declarations, so this module names their operation from
//! the complete declaration: the annotation, the qualified owner or package, the receiver, every
//! parameter, and the result. A same-named declaration with any other shape stays unclassified.

use crate::libraries::builtin_declaration::{BuiltinFunctionDeclaration, BuiltinMemberDeclaration};
use crate::libraries::FnKind;
use crate::types::{ConstNumericOperation, Ty, TypeName};

/// The operation of an annotated member of `UByte`, `UShort`, `UInt` or `ULong`.
pub(crate) fn member_operation(
    declaration: &BuiltinMemberDeclaration<'_>,
) -> Option<ConstNumericOperation> {
    if declaration.is_property
        || declaration.is_infix
        || !is_intrinsic_const(declaration.annotations)
    {
        return None;
    }
    let owner = unsigned_owner(declaration.owner)?;
    match (declaration.name, declaration.params) {
        (name, []) => (conversion_target(name) == Some(declaration.ret)
            && is_numeric(declaration.ret))
        .then_some(ConstNumericOperation::Conversion),
        ("compareTo", [other]) => (declaration.is_operator
            && is_unsigned(*other)
            && *other != owner
            && declaration.ret == Ty::Int)
            .then_some(ConstNumericOperation::CompareTo),
        (name, [other]) => {
            let operation = binary_operation(name, declaration.is_operator)?;
            (is_unsigned(*other) && is_unsigned(declaration.ret)).then_some(operation)
        }
        _ => None,
    }
}

/// The operation of an annotated `kotlin` package extension: a conversion of a signed value to an
/// unsigned type, or `floorDiv`/`mod` on signed values.
pub(crate) fn function_operation(
    declaration: &BuiltinFunctionDeclaration<'_>,
    annotations: &[TypeName],
) -> Option<ConstNumericOperation> {
    if declaration.package != crate::types::wk::kotlin_package()
        || declaration.kind != FnKind::Extension
        || declaration.context_count != 0
        || declaration.type_parameter_count != 0
        || declaration.vararg.is_some()
        || declaration.is_suspend
        || declaration.is_operator
        || declaration.is_infix
        || !is_intrinsic_const(annotations)
    {
        return None;
    }
    let receiver = declaration.receiver?;
    if !is_signed(receiver) {
        return None;
    }
    match (declaration.name, declaration.params) {
        (name, []) => (conversion_target(name) == Some(declaration.ret)
            && is_unsigned(declaration.ret))
        .then_some(ConstNumericOperation::Conversion),
        ("floorDiv", [other]) => {
            (is_integral(receiver) && is_integral(*other) && is_integral(declaration.ret))
                .then_some(ConstNumericOperation::FloorDivide)
        }
        ("mod", [other]) => {
            ((is_integral(receiver) && is_integral(*other) && is_integral(declaration.ret))
                || (is_floating(receiver) && is_floating(*other) && is_floating(declaration.ret)))
            .then_some(ConstNumericOperation::Modulo)
        }
        _ => None,
    }
}

fn is_intrinsic_const(annotations: &[TypeName]) -> bool {
    annotations.contains(&crate::types::wk::intrinsic_const_evaluation())
}

fn binary_operation(name: &str, is_operator: bool) -> Option<ConstNumericOperation> {
    Some(match (name, is_operator) {
        ("plus", true) => ConstNumericOperation::Add,
        ("minus", true) => ConstNumericOperation::Subtract,
        ("times", true) => ConstNumericOperation::Multiply,
        ("div", true) => ConstNumericOperation::Divide,
        ("rem", true) => ConstNumericOperation::Remainder,
        ("floorDiv", false) => ConstNumericOperation::FloorDivide,
        ("mod", false) => ConstNumericOperation::Modulo,
        _ => return None,
    })
}

/// The type a `toX()` conversion's name promises; the declaration's result must agree with it.
fn conversion_target(name: &str) -> Option<Ty> {
    Some(match name {
        "toByte" => Ty::Byte,
        "toShort" => Ty::Short,
        "toInt" => Ty::Int,
        "toLong" => Ty::Long,
        "toFloat" => Ty::Float,
        "toDouble" => Ty::Double,
        "toUByte" => Ty::UByte,
        "toUShort" => Ty::UShort,
        "toUInt" => Ty::UInt,
        "toULong" => Ty::ULong,
        _ => return None,
    })
}

fn unsigned_owner(owner: TypeName) -> Option<Ty> {
    use crate::types::wk;
    [
        (wk::ubyte(), Ty::UByte),
        (wk::ushort(), Ty::UShort),
        (wk::uint(), Ty::UInt),
        (wk::ulong(), Ty::ULong),
    ]
    .into_iter()
    .find_map(|(name, ty)| (name == owner).then_some(ty))
}

fn is_unsigned(ty: Ty) -> bool {
    matches!(ty, Ty::UByte | Ty::UShort | Ty::UInt | Ty::ULong)
}

fn is_integral(ty: Ty) -> bool {
    matches!(ty, Ty::Byte | Ty::Short | Ty::Int | Ty::Long)
}

fn is_floating(ty: Ty) -> bool {
    matches!(ty, Ty::Float | Ty::Double)
}

fn is_signed(ty: Ty) -> bool {
    is_integral(ty) || is_floating(ty)
}

fn is_numeric(ty: Ty) -> bool {
    is_signed(ty) || is_unsigned(ty)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn member<'a>(
        name: &'a str,
        params: &'a [Ty],
        ret: Ty,
        annotations: &'a [TypeName],
    ) -> BuiltinMemberDeclaration<'a> {
        BuiltinMemberDeclaration {
            owner: crate::types::wk::uint(),
            name,
            params,
            ret,
            is_property: false,
            is_operator: !name.starts_with("to"),
            is_infix: false,
            annotations,
        }
    }

    #[test]
    fn an_unsigned_member_needs_the_annotation_and_its_complete_shape() {
        let annotated = [crate::types::wk::intrinsic_const_evaluation()];
        assert_eq!(
            member_operation(&member("plus", &[Ty::UInt], Ty::UInt, &annotated)),
            Some(ConstNumericOperation::Add)
        );
        assert_eq!(
            member_operation(&member("plus", &[Ty::UInt], Ty::UInt, &[])),
            None
        );
        assert_eq!(
            member_operation(&member("plus", &[Ty::Int], Ty::UInt, &annotated)),
            None
        );
        assert_eq!(
            member_operation(&member("toLong", &[], Ty::Long, &annotated)),
            Some(ConstNumericOperation::Conversion)
        );
        assert_eq!(
            member_operation(&member("toLong", &[], Ty::Int, &annotated)),
            None
        );
        // `UInt.compareTo(UInt)` is the `Comparable` override, named by its own role.
        assert_eq!(
            member_operation(&member("compareTo", &[Ty::UInt], Ty::Int, &annotated)),
            None
        );
        assert_eq!(
            member_operation(&member("compareTo", &[Ty::UByte], Ty::Int, &annotated)),
            Some(ConstNumericOperation::CompareTo)
        );
    }
}
