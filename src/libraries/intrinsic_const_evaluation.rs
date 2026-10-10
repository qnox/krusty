//! The operations kotlinc's frontend evaluates in a constant expression: a port of its generated
//! `OperationsMap` (`evalUnaryOp`/`evalBinaryOp`) over typed constant values.
//!
//! kotlinc keys that table by the callable identity of a standard-library declaration and the
//! compile-time types of its receiver and first argument. That table (`known_operations.txt`, the
//! keys of kotlinc's generated `OperationsMapGenerated`) is the authority rather than the
//! `@kotlin.internal.IntrinsicConstEvaluation` annotation, which the JVM standard library does not
//! carry on every operation kotlinc folds (`trim`, `uppercase`, `Char(Int)`). Here a declaration
//! selected by resolution becomes an [`IntrinsicConstOperation`] through
//! [`compile_time_operation`]. The value semantics are Kotlin/JVM's: integral
//! arithmetic wraps, a floating value converts to an integral type rounding toward zero and
//! saturating, `compareTo` on floating values is the total order, and `equals` compares boxed
//! values (`0.0` is not `-0.0`, `NaN` is `NaN`).

use crate::kt_string::{KtString, KtStringBuf};
use crate::types::{Ty, TypeName};

use super::{LibConst, LibraryConst};

/// A compile-time constant of one of the types a constant expression may have (kotlinc's
/// `constantAllowedTypes`: the primitive types, the unsigned types and `String`).
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum ConstValue {
    Boolean(bool),
    Char(u16),
    Byte(i8),
    Short(i16),
    Int(i32),
    Long(i64),
    Float(f32),
    Double(f64),
    UByte(u8),
    UShort(u16),
    UInt(u32),
    ULong(u64),
    String(KtString),
}

impl ConstValue {
    /// The Kotlin type of this value.
    pub(crate) fn ty(&self) -> Ty {
        match self {
            Self::Boolean(_) => Ty::Boolean,
            Self::Char(_) => Ty::Char,
            Self::Byte(_) => Ty::Byte,
            Self::Short(_) => Ty::Short,
            Self::Int(_) => Ty::Int,
            Self::Long(_) => Ty::Long,
            Self::Float(_) => Ty::Float,
            Self::Double(_) => Ty::Double,
            Self::UByte(_) => Ty::UByte,
            Self::UShort(_) => Ty::UShort,
            Self::UInt(_) => Ty::UInt,
            Self::ULong(_) => Ty::ULong,
            Self::String(_) => Ty::String,
        }
    }

    /// The value of a declaration-owned constant payload, read at the payload's own type.
    pub(crate) fn from_library(constant: &LibraryConst) -> Option<Self> {
        let ty = constant.ty.non_null().canonical_semantic();
        Some(match (&constant.value, ty) {
            (LibConst::Int(value), Ty::Boolean) => Self::Boolean(*value != 0),
            (LibConst::Int(value), Ty::Char) => Self::Char(*value as u16),
            (LibConst::Int(value), Ty::Byte) => Self::Byte(*value as i8),
            (LibConst::Int(value), Ty::Short) => Self::Short(*value as i16),
            (LibConst::Int(value), Ty::Int) => Self::Int(*value),
            (LibConst::Int(value), Ty::UByte) => Self::UByte(*value as u8),
            (LibConst::Int(value), Ty::UShort) => Self::UShort(*value as u16),
            (LibConst::Int(value), Ty::UInt) => Self::UInt(*value as u32),
            (LibConst::Long(value), Ty::Long) => Self::Long(*value),
            (LibConst::Long(value), Ty::ULong) => Self::ULong(*value as u64),
            (LibConst::Float(value), Ty::Float) => Self::Float(*value),
            (LibConst::Double(value), Ty::Double) => Self::Double(*value),
            (LibConst::Str(value), Ty::String) => Self::String(value.clone()),
            _ => return None,
        })
    }

    /// This value as a declaration-owned payload, in the representation the platform stores it:
    /// the narrow integral types, `Char`, `Boolean` and the 32-bit unsigned types as an `Int` bit
    /// pattern, `ULong` as a `Long` one.
    pub(crate) fn into_library(self) -> LibraryConst {
        let ty = self.ty();
        let value = match self {
            Self::Boolean(value) => LibConst::Int(i32::from(value)),
            Self::Char(value) => LibConst::Int(i32::from(value)),
            Self::Byte(value) => LibConst::Int(i32::from(value)),
            Self::Short(value) => LibConst::Int(i32::from(value)),
            Self::Int(value) => LibConst::Int(value),
            Self::UByte(value) => LibConst::Int(i32::from(value as i8)),
            Self::UShort(value) => LibConst::Int(i32::from(value as i16)),
            Self::UInt(value) => LibConst::Int(value as i32),
            Self::Long(value) => LibConst::Long(value),
            Self::ULong(value) => LibConst::Long(value as i64),
            Self::Float(value) => LibConst::Float(value),
            Self::Double(value) => LibConst::Double(value),
            Self::String(value) => LibConst::Str(value),
        };
        LibraryConst { ty, value }
    }

    /// kotlinc's `convertToGivenKind` towards `ty`: a `Boolean`, `Char` or `String` only as itself,
    /// a signed or floating number converted as `Number.toX()` does, and an unsigned type from
    /// itself or from a signed or floating number through `toLong()`. `None` when `ty` is not a
    /// constant type or the value has no such conversion.
    pub(crate) fn converted_to(&self, ty: Ty) -> Option<Self> {
        let ty = ty.non_null().canonical_semantic();
        if self.ty() == ty {
            return Some(self.clone());
        }
        let number = !matches!(
            self,
            Self::Boolean(_)
                | Self::Char(_)
                | Self::String(_)
                | Self::UByte(_)
                | Self::UShort(_)
                | Self::UInt(_)
                | Self::ULong(_)
        );
        if !number {
            return None;
        }
        match ty {
            Ty::Byte | Ty::Short | Ty::Int | Ty::Long | Ty::Float | Ty::Double => convert(self, ty),
            Ty::UByte | Ty::UShort | Ty::UInt | Ty::ULong => {
                Some(wrap(i128::from(as_i64(self)?), ty))
            }
            _ => None,
        }
    }

    /// Append Kotlin's `toString()` text of this value.
    pub(crate) fn push_text(&self, output: &mut KtStringBuf) -> Option<()> {
        match self {
            Self::Boolean(value) => output.push_str(if *value { "true" } else { "false" }),
            Self::Char(value) => output.push_unit(*value),
            Self::Byte(value) => output.push_str(&value.to_string()),
            Self::Short(value) => output.push_str(&value.to_string()),
            Self::Int(value) => output.push_str(&value.to_string()),
            Self::Long(value) => output.push_str(&value.to_string()),
            Self::UByte(value) => output.push_str(&value.to_string()),
            Self::UShort(value) => output.push_str(&value.to_string()),
            Self::UInt(value) => output.push_str(&value.to_string()),
            Self::ULong(value) => output.push_str(&value.to_string()),
            Self::Float(value) => crate::kt_string::push_f32(*value, output)?,
            Self::Double(value) => crate::kt_string::push_f64(*value, output)?,
            Self::String(value) => output.push_kt(value),
        }
        Some(())
    }

    /// Kotlin's `equals` between two boxed constants: values of different types are never equal,
    /// and floating values compare by their bits with every `NaN` alike.
    pub(crate) fn kotlin_equals(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Float(left), Self::Float(right)) => {
                canonical_f32(*left) == canonical_f32(*right)
            }
            (Self::Double(left), Self::Double(right)) => {
                canonical_f64(*left) == canonical_f64(*right)
            }
            _ => self == other,
        }
    }

    fn text(&self) -> Option<KtString> {
        let mut output = KtStringBuf::new();
        self.push_text(&mut output)?;
        Some(output.finish())
    }
}

fn canonical_f32(value: f32) -> u32 {
    if value.is_nan() {
        f32::NAN.to_bits()
    } else {
        value.to_bits()
    }
}

fn canonical_f64(value: f64) -> u64 {
    if value.is_nan() {
        f64::NAN.to_bits()
    } else {
        value.to_bits()
    }
}

/// A standard-library operation a constant expression may apply, named after the declaration
/// kotlinc's operation table keys it by.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum IntrinsicConstOperation {
    Not,
    Inc,
    Dec,
    Inv,
    UnaryMinus,
    UnaryPlus,
    /// `toByte()` … `toDouble()`, `toChar()`, and the unsigned `toUByte()` … `toULong()`.
    Convert(Ty),
    ToString,
    /// `Char.code`.
    Code,
    /// `String.length`.
    Length,
    Lowercase,
    Uppercase,
    Trim,
    TrimStart,
    TrimEnd,
    TrimIndent,
    TrimMargin,
    /// `Char(code: Int)`.
    CharOfCode,
    Plus,
    Minus,
    Times,
    Div,
    Rem,
    FloorDiv,
    Mod,
    And,
    Or,
    Xor,
    Shl,
    Shr,
    Ushr,
    CompareTo,
    Equals,
    Get,
}

impl IntrinsicConstOperation {
    fn named(name: &str) -> Option<Self> {
        Some(match name {
            "not" => Self::Not,
            "inc" => Self::Inc,
            "dec" => Self::Dec,
            "inv" => Self::Inv,
            "unaryMinus" => Self::UnaryMinus,
            "unaryPlus" => Self::UnaryPlus,
            "toByte" => Self::Convert(Ty::Byte),
            "toShort" => Self::Convert(Ty::Short),
            "toInt" => Self::Convert(Ty::Int),
            "toLong" => Self::Convert(Ty::Long),
            "toFloat" => Self::Convert(Ty::Float),
            "toDouble" => Self::Convert(Ty::Double),
            "toChar" => Self::Convert(Ty::Char),
            "toUByte" => Self::Convert(Ty::UByte),
            "toUShort" => Self::Convert(Ty::UShort),
            "toUInt" => Self::Convert(Ty::UInt),
            "toULong" => Self::Convert(Ty::ULong),
            "toString" => Self::ToString,
            "code" => Self::Code,
            "length" => Self::Length,
            "lowercase" => Self::Lowercase,
            "uppercase" => Self::Uppercase,
            "trim" => Self::Trim,
            "trimStart" => Self::TrimStart,
            "trimEnd" => Self::TrimEnd,
            "trimIndent" => Self::TrimIndent,
            "trimMargin" => Self::TrimMargin,
            "Char" => Self::CharOfCode,
            "plus" => Self::Plus,
            "minus" => Self::Minus,
            "times" => Self::Times,
            "div" => Self::Div,
            "rem" => Self::Rem,
            "floorDiv" => Self::FloorDiv,
            "mod" => Self::Mod,
            "and" => Self::And,
            "or" => Self::Or,
            "xor" => Self::Xor,
            "shl" => Self::Shl,
            "shr" => Self::Shr,
            "ushr" => Self::Ushr,
            "compareTo" => Self::CompareTo,
            "equals" => Self::Equals,
            "get" => Self::Get,
            _ => return None,
        })
    }

    /// Whether kotlinc evaluates a `kotlin` package call of this operation without
    /// `IntrinsicConstEvaluation`: the simple unary, binary and bitwise operators, `compareTo`,
    /// `floorDiv`, `mod`, `code`, `toString`, the number conversions, and `String.get`.
    fn is_legacy_compile_time(self) -> bool {
        match self {
            Self::Convert(target) => !target.is_unsigned(),
            Self::UnaryPlus
            | Self::UnaryMinus
            | Self::Not
            | Self::Inv
            | Self::Plus
            | Self::Minus
            | Self::Times
            | Self::Div
            | Self::Rem
            | Self::And
            | Self::Or
            | Self::Xor
            | Self::Shl
            | Self::Shr
            | Self::Ushr
            | Self::CompareTo
            | Self::FloorDiv
            | Self::Mod
            | Self::Code
            | Self::ToString
            | Self::Get => true,
            _ => false,
        }
    }
}

/// Who declares a selected callable.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum IntrinsicConstOwner {
    /// A member of the builtin classifier of this constant type (every constant type's classifier
    /// is declared in package `kotlin`).
    Classifier(Ty),
    /// A package-level function or extension.
    Package(TypeName),
}

/// The declaration facts that decide whether a selected standard-library callable is a
/// compile-time operation: kotlinc's `CallableId` and the compile-time types of the declared
/// receiver and first value parameter.
#[derive(Clone, Copy, Debug)]
pub(crate) struct IntrinsicConstDeclaration<'a> {
    pub owner: IntrinsicConstOwner,
    /// The declaration's Kotlin name.
    pub name: &'a str,
    /// The declared dispatch or extension receiver type.
    pub receiver: Option<Ty>,
    /// The declared type of the first value parameter.
    pub first_parameter: Option<Ty>,
}

/// kotlinc's `CompileTimeType`: how its operation table names an operand type.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum CompileTimeType {
    Byte,
    Short,
    Int,
    Long,
    UByte,
    UShort,
    UInt,
    ULong,
    Double,
    Float,
    Char,
    Boolean,
    String,
    Any,
}

impl CompileTimeType {
    fn of(ty: Ty) -> Option<Self> {
        let ty = ty.non_null().canonical_semantic();
        Some(match ty {
            Ty::Byte => Self::Byte,
            Ty::Short => Self::Short,
            Ty::Int => Self::Int,
            Ty::Long => Self::Long,
            Ty::UByte => Self::UByte,
            Ty::UShort => Self::UShort,
            Ty::UInt => Self::UInt,
            Ty::ULong => Self::ULong,
            Ty::Double => Self::Double,
            Ty::Float => Self::Float,
            Ty::Char => Self::Char,
            Ty::Boolean => Self::Boolean,
            Ty::String => Self::String,
            ty if ty.obj_internal() == Some(crate::types::wk::any()) => Self::Any,
            _ => return None,
        })
    }

    fn named(name: &str) -> Option<Self> {
        Some(match name {
            "BYTE" => Self::Byte,
            "SHORT" => Self::Short,
            "INT" => Self::Int,
            "LONG" => Self::Long,
            "UBYTE" => Self::UByte,
            "USHORT" => Self::UShort,
            "UINT" => Self::UInt,
            "ULONG" => Self::ULong,
            "DOUBLE" => Self::Double,
            "FLOAT" => Self::Float,
            "CHAR" => Self::Char,
            "BOOLEAN" => Self::Boolean,
            "STRING" => Self::String,
            "ANY" => Self::Any,
            _ => return None,
        })
    }
}

/// One entry of kotlinc's operation table.
type KnownOperation = (IntrinsicConstOwner, String, Vec<CompileTimeType>);

/// kotlinc's generated table of the standard-library operations a constant expression may apply
/// under `IntrinsicConstEvaluation` (`OperationsMapGenerated.knownOps`), as declaration identities.
///
/// kotlinc keys it by `CallableId` rather than reading `@IntrinsicConstEvaluation` from the
/// library: several listed JVM declarations (`trim`, `uppercase`, `Char(Int)`) carry no such
/// annotation in `kotlin-stdlib`.
fn known_operations() -> &'static std::collections::HashSet<KnownOperation> {
    static KNOWN: std::sync::OnceLock<std::collections::HashSet<KnownOperation>> =
        std::sync::OnceLock::new();
    KNOWN.get_or_init(|| {
        include_str!("intrinsic_const_evaluation/known_operations.txt")
            .lines()
            .filter(|line| !line.starts_with('#') && !line.is_empty())
            .map(|line| {
                let (callable, types) = line
                    .strip_suffix(')')
                    .and_then(|line| line.split_once('('))
                    .expect("an operation table entry is `callable(types)`");
                let types = types
                    .split(", ")
                    .map(|name| CompileTimeType::named(name).expect("a compile-time type"))
                    .collect::<Vec<_>>();
                let (owner, name) = match callable.rsplit_once('.') {
                    Some((classifier, name)) => (
                        IntrinsicConstOwner::Classifier(
                            Ty::obj_name(crate::types::type_name(classifier)).canonical_semantic(),
                        ),
                        name,
                    ),
                    None => {
                        let (package, name) = callable
                            .rsplit_once('/')
                            .expect("a package-level operation names its package");
                        (
                            IntrinsicConstOwner::Package(crate::types::type_name(package)),
                            name,
                        )
                    }
                };
                (owner, name.to_string(), types)
            })
            .collect()
    })
}

/// The operation kotlinc evaluates for a call of `declaration` in a constant expression, or `None`
/// when the call is not a compile-time one (kotlinc's `isCompileTimeBuiltinCall`).
///
/// With `IntrinsicConstEvaluation`, a declaration in kotlinc's operation table is one. Without it,
/// only a declaration of the `kotlin` package itself is, when it names a legacy operation, `get`
/// is that of a `String`, and its dispatch receiver is not unsigned.
pub(crate) fn compile_time_operation(
    declaration: IntrinsicConstDeclaration<'_>,
    intrinsic_const_evaluation: bool,
) -> Option<IntrinsicConstOperation> {
    let kotlin = crate::types::wk::kotlin_package();
    let package = match declaration.owner {
        IntrinsicConstOwner::Classifier(_) => kotlin,
        IntrinsicConstOwner::Package(package) => package,
    };
    let from_stdlib = std::iter::successors(Some(package), |package| package.parent())
        .any(|package| package == kotlin);
    if !from_stdlib {
        return None;
    }
    let operation = IntrinsicConstOperation::named(declaration.name)?;
    if intrinsic_const_evaluation {
        let types = [declaration.receiver, declaration.first_parameter]
            .into_iter()
            .flatten()
            .map(CompileTimeType::of)
            .collect::<Option<Vec<_>>>()?;
        let key = (declaration.owner, declaration.name.to_string(), types);
        return known_operations().contains(&key).then_some(operation);
    }
    let dispatch_receiver = match declaration.owner {
        IntrinsicConstOwner::Classifier(receiver) => Some(receiver),
        IntrinsicConstOwner::Package(_) => None,
    };
    let legacy = package == kotlin
        && operation.is_legacy_compile_time()
        && !dispatch_receiver.is_some_and(Ty::is_unsigned)
        && (operation != IntrinsicConstOperation::Get || dispatch_receiver == Some(Ty::String));
    legacy.then_some(operation)
}

/// Why an operation over constants has no value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum OperationFailure {
    /// The operation does not apply to these operands, or it throws.
    NotConst,
    /// An integral `div` or `rem` by zero (kotlinc's `DivisionByZero`).
    DivisionByZero,
}

/// Apply `operation` to `operands` (the receiver first) and convert the value to the call's
/// `result` type, as kotlinc's `adjustTypeAndConvertToLiteral` does.
pub(crate) fn evaluate(
    operation: IntrinsicConstOperation,
    operands: &[ConstValue],
    result: Ty,
) -> Result<ConstValue, OperationFailure> {
    let value = match operands {
        [operand] => unary(operation, operand),
        [left, right] => binary(operation, left, right)?,
        _ => None,
    };
    value
        .and_then(|value| value.converted_to(result))
        .ok_or(OperationFailure::NotConst)
}

fn unary(operation: IntrinsicConstOperation, operand: &ConstValue) -> Option<ConstValue> {
    use IntrinsicConstOperation as Op;
    Some(match (operation, operand) {
        (Op::Not, ConstValue::Boolean(value)) => ConstValue::Boolean(!value),
        (Op::Inv, value) if is_integral(value) => wrap(!integral(value)?, value.ty()),
        (Op::UnaryMinus | Op::UnaryPlus, value) => {
            let negate = operation == Op::UnaryMinus;
            match value {
                ConstValue::Float(value) => ConstValue::Float(if negate { -value } else { *value }),
                ConstValue::Double(value) => {
                    ConstValue::Double(if negate { -value } else { *value })
                }
                ConstValue::Byte(_) | ConstValue::Short(_) | ConstValue::Int(_) => {
                    let value = integral(value)?;
                    wrap(if negate { -value } else { value }, Ty::Int)
                }
                ConstValue::Long(_) => {
                    let value = integral(value)?;
                    wrap(if negate { -value } else { value }, Ty::Long)
                }
                _ => return None,
            }
        }
        (Op::Inc | Op::Dec, value) => {
            let step = if operation == Op::Inc { 1 } else { -1 };
            match value {
                ConstValue::Float(value) => ConstValue::Float(value + step as f32),
                ConstValue::Double(value) => ConstValue::Double(value + f64::from(step)),
                ConstValue::Char(code) => ConstValue::Char((i32::from(*code) + step) as u16),
                value if is_integral(value) => {
                    wrap(integral(value)? + i128::from(step), value.ty())
                }
                _ => return None,
            }
        }
        (Op::Convert(target), value) => convert(value, target)?,
        (Op::ToString, value) => ConstValue::String(value.text()?),
        (Op::Code, ConstValue::Char(code)) => ConstValue::Int(i32::from(*code)),
        (Op::Length, ConstValue::String(text)) => {
            ConstValue::Int(i32::try_from(text.len_utf16()).ok()?)
        }
        (Op::Lowercase, ConstValue::String(text)) => {
            ConstValue::String(crate::kt_string::lowercase(text))
        }
        (Op::Uppercase, ConstValue::String(text)) => {
            ConstValue::String(crate::kt_string::uppercase(text))
        }
        (Op::Trim, ConstValue::String(text)) => ConstValue::String(crate::kt_string::trim(text)),
        (Op::TrimStart, ConstValue::String(text)) => {
            ConstValue::String(crate::kt_string::trim_start(text))
        }
        (Op::TrimEnd, ConstValue::String(text)) => {
            ConstValue::String(crate::kt_string::trim_end(text))
        }
        (Op::TrimIndent, ConstValue::String(text)) => {
            ConstValue::String(crate::kt_string::trim_indent(text))
        }
        (Op::TrimMargin, ConstValue::String(text)) => {
            ConstValue::String(crate::kt_string::trim_margin(text, &KtString::from("|")))
        }
        (Op::CharOfCode, ConstValue::Int(code)) => ConstValue::Char(u16::try_from(*code).ok()?),
        _ => return None,
    })
}

fn binary(
    operation: IntrinsicConstOperation,
    left: &ConstValue,
    right: &ConstValue,
) -> Result<Option<ConstValue>, OperationFailure> {
    use IntrinsicConstOperation as Op;
    let value = match (operation, left, right) {
        (Op::Equals, left, right) => Some(ConstValue::Boolean(left.kotlin_equals(right))),
        (Op::Plus, ConstValue::String(text), right) => {
            let mut output = KtStringBuf::new();
            output.push_kt(text);
            right.push_text(&mut output);
            Some(ConstValue::String(output.finish()))
        }
        (Op::Get, ConstValue::String(text), ConstValue::Int(index)) => usize::try_from(*index)
            .ok()
            .and_then(|index| text.units().nth(index))
            .map(ConstValue::Char),
        (Op::TrimMargin, ConstValue::String(text), ConstValue::String(prefix)) => {
            let blank = prefix
                .units()
                .all(|unit| crate::kt_string::trim(&KtString::from_units(vec![unit])).is_empty());
            (!blank).then(|| ConstValue::String(crate::kt_string::trim_margin(text, prefix)))
        }
        (Op::CompareTo, left, right) => compare(left, right).map(ConstValue::Int),
        (Op::And | Op::Or | Op::Xor, ConstValue::Boolean(left), ConstValue::Boolean(right)) => {
            Some(ConstValue::Boolean(match operation {
                Op::And => left & right,
                Op::Or => left | right,
                _ => left ^ right,
            }))
        }
        (Op::And | Op::Or | Op::Xor, left, right) if left.ty() == right.ty() => {
            let (lhs, rhs) = (integral(left), integral(right));
            match (lhs, rhs) {
                (Some(lhs), Some(rhs)) => Some(wrap(
                    match operation {
                        Op::And => lhs & rhs,
                        Op::Or => lhs | rhs,
                        _ => lhs ^ rhs,
                    },
                    left.ty(),
                )),
                _ => None,
            }
        }
        (Op::Shl | Op::Shr | Op::Ushr, left, ConstValue::Int(distance)) => {
            shift(operation, left, *distance)
        }
        (Op::Plus | Op::Minus, ConstValue::Char(code), ConstValue::Int(offset)) => {
            let code = i64::from(*code);
            let offset = i64::from(*offset);
            Some(ConstValue::Char(if operation == Op::Plus {
                code.wrapping_add(offset) as u16
            } else {
                code.wrapping_sub(offset) as u16
            }))
        }
        (Op::Minus, ConstValue::Char(left), ConstValue::Char(right)) => {
            Some(ConstValue::Int(i32::from(*left) - i32::from(*right)))
        }
        (Op::Plus | Op::Minus | Op::Times | Op::Div | Op::Rem | Op::FloorDiv | Op::Mod, _, _) => {
            return arithmetic(operation, left, right)
        }
        _ => None,
    };
    Ok(value)
}

/// The type both operands of a numeric operation are promoted to: `Double`, `Float` or `Long` when
/// either operand has it, else `Int`; among unsigned operands `ULong` or else `UInt`.
fn promoted(left: &ConstValue, right: &ConstValue) -> Option<Ty> {
    let (left, right) = (left.ty(), right.ty());
    if left.is_numeric() && right.is_numeric() {
        return Some(
            [Ty::Double, Ty::Float, Ty::Long]
                .into_iter()
                .find(|ty| left == *ty || right == *ty)
                .unwrap_or(Ty::Int),
        );
    }
    if left.is_unsigned() && right.is_unsigned() {
        return Some(if left == Ty::ULong || right == Ty::ULong {
            Ty::ULong
        } else {
            Ty::UInt
        });
    }
    None
}

fn arithmetic(
    operation: IntrinsicConstOperation,
    left: &ConstValue,
    right: &ConstValue,
) -> Result<Option<ConstValue>, OperationFailure> {
    use IntrinsicConstOperation as Op;
    let Some(domain) = promoted(left, right) else {
        return Ok(None);
    };
    // `mod` takes the divisor's type: `Int.mod(Byte): Byte`, `Long.mod(Int): Int`.
    let natural = if operation == Op::Mod && domain != Ty::Float && domain != Ty::Double {
        right.ty()
    } else {
        domain
    };
    let value = match domain {
        Ty::Float => {
            let (Some(lhs), Some(rhs)) = (as_f32(left), as_f32(right)) else {
                return Ok(None);
            };
            floating(operation, f64::from(lhs), f64::from(rhs)).map(|value| {
                if operation == Op::Mod && right.ty() == Ty::Double {
                    ConstValue::Double(value)
                } else {
                    ConstValue::Float(value as f32)
                }
            })
        }
        Ty::Double => {
            let (Some(lhs), Some(rhs)) = (as_f64(left), as_f64(right)) else {
                return Ok(None);
            };
            floating(operation, lhs, rhs).map(ConstValue::Double)
        }
        _ => {
            let (Some(lhs), Some(rhs)) = (integral(left), integral(right)) else {
                return Ok(None);
            };
            if rhs == 0 && matches!(operation, Op::Div | Op::Rem | Op::FloorDiv | Op::Mod) {
                let signed = !domain.is_unsigned();
                return if signed && matches!(operation, Op::Div | Op::Rem) {
                    Err(OperationFailure::DivisionByZero)
                } else {
                    Ok(None)
                };
            }
            let (lhs, rhs) = (wrapped(lhs, domain), wrapped(rhs, domain));
            let value = match operation {
                Op::Plus => lhs + rhs,
                Op::Minus => lhs - rhs,
                Op::Times => lhs * rhs,
                Op::Div => lhs / rhs,
                Op::Rem => lhs % rhs,
                Op::FloorDiv => {
                    lhs.div_euclid(rhs) - i128::from(rhs < 0 && lhs.rem_euclid(rhs) != 0)
                }
                Op::Mod => {
                    let remainder = lhs % rhs;
                    if remainder != 0 && (remainder < 0) != (rhs < 0) {
                        remainder + rhs
                    } else {
                        remainder
                    }
                }
                _ => return Ok(None),
            };
            Some(wrap(value, natural))
        }
    };
    Ok(value)
}

/// A `Float` operation is computed in `f64` and rounded once: every operation here is exactly
/// representable there, so the single rounding is the `Float` result.
fn floating(operation: IntrinsicConstOperation, left: f64, right: f64) -> Option<f64> {
    use IntrinsicConstOperation as Op;
    Some(match operation {
        Op::Plus => left + right,
        Op::Minus => left - right,
        Op::Times => left * right,
        Op::Div => left / right,
        Op::Rem => left % right,
        Op::Mod => {
            let remainder = left % right;
            if remainder != 0.0 && (remainder < 0.0) != (right < 0.0) {
                remainder + right
            } else {
                remainder
            }
        }
        _ => return None,
    })
}

fn shift(
    operation: IntrinsicConstOperation,
    left: &ConstValue,
    distance: i32,
) -> Option<ConstValue> {
    use IntrinsicConstOperation as Op;
    let distance = distance as u32;
    Some(match left {
        ConstValue::Int(value) => ConstValue::Int(match operation {
            Op::Shl => value.wrapping_shl(distance),
            Op::Shr => value.wrapping_shr(distance),
            _ => (*value as u32).wrapping_shr(distance) as i32,
        }),
        ConstValue::Long(value) => ConstValue::Long(match operation {
            Op::Shl => value.wrapping_shl(distance),
            Op::Shr => value.wrapping_shr(distance),
            _ => (*value as u64).wrapping_shr(distance) as i64,
        }),
        ConstValue::UInt(value) => ConstValue::UInt(match operation {
            Op::Shl => value.wrapping_shl(distance),
            Op::Shr => value.wrapping_shr(distance),
            _ => return None,
        }),
        ConstValue::ULong(value) => ConstValue::ULong(match operation {
            Op::Shl => value.wrapping_shl(distance),
            Op::Shr => value.wrapping_shr(distance),
            _ => return None,
        }),
        _ => return None,
    })
}

/// `compareTo`: `-1`, `0` or `1` for numbers, characters and booleans (floating values in their
/// total order), and Java's code-unit difference for strings.
fn compare(left: &ConstValue, right: &ConstValue) -> Option<i32> {
    let order = match (left, right) {
        (ConstValue::Boolean(left), ConstValue::Boolean(right)) => left.cmp(right),
        (ConstValue::Char(left), ConstValue::Char(right)) => left.cmp(right),
        (ConstValue::String(left), ConstValue::String(right)) => {
            let mut left = left.units();
            let mut right = right.units();
            loop {
                match (left.next(), right.next()) {
                    (Some(l), Some(r)) if l == r => continue,
                    (Some(l), Some(r)) => return Some(i32::from(l) - i32::from(r)),
                    (Some(_), None) => return Some(1 + left.count() as i32),
                    (None, Some(_)) => return Some(-1 - right.count() as i32),
                    (None, None) => return Some(0),
                }
            }
        }
        _ => match promoted(left, right)? {
            Ty::Double => total_order(as_f64(left)?, as_f64(right)?),
            Ty::Float => total_order(f64::from(as_f32(left)?), f64::from(as_f32(right)?)),
            _ => integral(left)?.cmp(&integral(right)?),
        },
    };
    Some(order as i32)
}

/// Java's `Double.compare`: `-0.0` below `0.0` and every `NaN` above positive infinity.
fn total_order(left: f64, right: f64) -> std::cmp::Ordering {
    match (left.is_nan(), right.is_nan()) {
        (true, true) => std::cmp::Ordering::Equal,
        (true, false) => std::cmp::Ordering::Greater,
        (false, true) => std::cmp::Ordering::Less,
        (false, false) => left.total_cmp(&right),
    }
}

fn is_integral(value: &ConstValue) -> bool {
    integral(value).is_some()
}

/// An integral value: signed types sign-extended, unsigned types zero-extended.
fn integral(value: &ConstValue) -> Option<i128> {
    Some(match value {
        ConstValue::Byte(value) => i128::from(*value),
        ConstValue::Short(value) => i128::from(*value),
        ConstValue::Int(value) => i128::from(*value),
        ConstValue::Long(value) => i128::from(*value),
        ConstValue::UByte(value) => i128::from(*value),
        ConstValue::UShort(value) => i128::from(*value),
        ConstValue::UInt(value) => i128::from(*value),
        ConstValue::ULong(value) => i128::from(*value),
        _ => return None,
    })
}

/// `value` as a value of `ty`, by the identity a wrapping conversion preserves.
fn wrapped(value: i128, ty: Ty) -> i128 {
    integral(&wrap(value, ty)).unwrap_or(value)
}

/// The integral (or `Char`) value of `ty` with the low bits of `value`.
fn wrap(value: i128, ty: Ty) -> ConstValue {
    match ty {
        Ty::Byte => ConstValue::Byte(value as i8),
        Ty::Short => ConstValue::Short(value as i16),
        Ty::Long => ConstValue::Long(value as i64),
        Ty::Char => ConstValue::Char(value as u16),
        Ty::UByte => ConstValue::UByte(value as u8),
        Ty::UShort => ConstValue::UShort(value as u16),
        Ty::UInt => ConstValue::UInt(value as u32),
        Ty::ULong => ConstValue::ULong(value as u64),
        _ => ConstValue::Int(value as i32),
    }
}

fn as_i64(value: &ConstValue) -> Option<i64> {
    Some(match value {
        ConstValue::Float(value) => *value as i64,
        ConstValue::Double(value) => *value as i64,
        value => integral(value)? as i64,
    })
}

/// `toFloat()`: an unsigned value converts through `toDouble()`, as the library defines it.
fn as_f32(value: &ConstValue) -> Option<f32> {
    Some(match value {
        ConstValue::Float(value) => *value,
        ConstValue::Double(value) => *value as f32,
        ConstValue::Byte(value) => f32::from(*value),
        ConstValue::Short(value) => f32::from(*value),
        ConstValue::Int(value) => *value as f32,
        ConstValue::Long(value) => *value as f32,
        ConstValue::Char(value) => f32::from(*value),
        value => as_f64(value)? as f32,
    })
}

fn as_f64(value: &ConstValue) -> Option<f64> {
    Some(match value {
        ConstValue::Float(value) => f64::from(*value),
        ConstValue::Double(value) => *value,
        ConstValue::Char(value) => f64::from(*value),
        ConstValue::Long(value) => *value as f64,
        ConstValue::ULong(value) => *value as f64,
        value => integral(value)? as f64,
    })
}

/// `toX()` from a number, a `Char` or an unsigned value. A floating value converts to `Long`,
/// `UInt` or `ULong` saturating and to the narrower types through `toInt()`.
fn convert(value: &ConstValue, target: Ty) -> Option<ConstValue> {
    if matches!(value, ConstValue::Boolean(_) | ConstValue::String(_)) {
        return None;
    }
    let floating = matches!(value, ConstValue::Float(_) | ConstValue::Double(_));
    let whole = |value: &ConstValue| -> Option<i128> {
        match value {
            ConstValue::Char(code) => Some(i128::from(*code)),
            ConstValue::Float(value) => Some(i128::from(*value as i32)),
            ConstValue::Double(value) => Some(i128::from(*value as i32)),
            value => integral(value),
        }
    };
    Some(match target {
        Ty::Float => ConstValue::Float(as_f32(value)?),
        Ty::Double => ConstValue::Double(as_f64(value)?),
        Ty::Long if floating => ConstValue::Long(as_f64(value)? as i64),
        Ty::UInt if floating => ConstValue::UInt(as_f64(value)? as u32),
        Ty::ULong if floating => ConstValue::ULong(as_f64(value)? as u64),
        Ty::UByte | Ty::UShort | Ty::UInt | Ty::ULong
            if matches!(value, ConstValue::Char(_)) || floating =>
        {
            return None
        }
        Ty::Char if value.ty().is_unsigned() => return None,
        Ty::Byte
        | Ty::Short
        | Ty::Int
        | Ty::Long
        | Ty::Char
        | Ty::UByte
        | Ty::UShort
        | Ty::UInt
        | Ty::ULong => wrap(whole(value)?, target),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use IntrinsicConstOperation as Op;

    fn eval(operation: IntrinsicConstOperation, operands: &[ConstValue], result: Ty) -> ConstValue {
        evaluate(operation, operands, result).expect("a constant")
    }

    #[test]
    fn arithmetic_wraps_in_the_promoted_type_and_mod_takes_the_divisor_type() {
        assert_eq!(
            eval(
                Op::Plus,
                &[ConstValue::Int(i32::MAX), ConstValue::Int(1)],
                Ty::Int
            ),
            ConstValue::Int(i32::MIN)
        );
        assert_eq!(
            eval(
                Op::Plus,
                &[ConstValue::Byte(100), ConstValue::Byte(100)],
                Ty::Int
            ),
            ConstValue::Int(200)
        );
        assert_eq!(
            eval(
                Op::Mod,
                &[ConstValue::Int(-7), ConstValue::Byte(3)],
                Ty::Byte
            ),
            ConstValue::Byte(2)
        );
        assert_eq!(
            eval(
                Op::FloorDiv,
                &[ConstValue::Int(-7), ConstValue::Long(2)],
                Ty::Long
            ),
            ConstValue::Long(-4)
        );
        assert_eq!(
            eval(
                Op::Div,
                &[ConstValue::UInt(u32::MAX), ConstValue::UInt(2)],
                Ty::UInt
            ),
            ConstValue::UInt(u32::MAX / 2)
        );
        assert_eq!(
            evaluate(Op::Div, &[ConstValue::Int(1), ConstValue::Int(0)], Ty::Int),
            Err(OperationFailure::DivisionByZero)
        );
        assert_eq!(
            evaluate(
                Op::Div,
                &[ConstValue::UInt(1), ConstValue::UInt(0)],
                Ty::UInt
            ),
            Err(OperationFailure::NotConst)
        );
    }

    #[test]
    fn comparisons_and_equality_follow_boxed_kotlin_values() {
        assert_eq!(
            eval(
                Op::CompareTo,
                &[ConstValue::Double(-0.0), ConstValue::Double(0.0)],
                Ty::Int
            ),
            ConstValue::Int(-1)
        );
        assert_eq!(
            eval(
                Op::CompareTo,
                &[ConstValue::Int(16_777_217), ConstValue::Float(16_777_216.0)],
                Ty::Int
            ),
            ConstValue::Int(0)
        );
        assert!(!ConstValue::Double(0.0).kotlin_equals(&ConstValue::Double(-0.0)));
        assert!(ConstValue::Double(f64::NAN).kotlin_equals(&ConstValue::Double(-f64::NAN)));
        assert!(!ConstValue::Int(1).kotlin_equals(&ConstValue::Long(1)));
        assert_eq!(
            eval(
                Op::CompareTo,
                &[
                    ConstValue::String(KtString::from("a")),
                    ConstValue::String(KtString::from("c"))
                ],
                Ty::Int
            ),
            ConstValue::Int(-2)
        );
    }

    #[test]
    fn conversions_are_kotlin_conversions() {
        assert_eq!(
            eval(Op::Convert(Ty::UInt), &[ConstValue::Byte(-1)], Ty::UInt),
            ConstValue::UInt(u32::MAX)
        );
        assert_eq!(
            eval(Op::Convert(Ty::Int), &[ConstValue::UByte(255)], Ty::Int),
            ConstValue::Int(255)
        );
        assert_eq!(
            eval(
                Op::Convert(Ty::Byte),
                &[ConstValue::Double(300.7)],
                Ty::Byte
            ),
            ConstValue::Byte(44)
        );
        assert_eq!(
            eval(Op::Convert(Ty::UInt), &[ConstValue::Double(-3.0)], Ty::UInt),
            ConstValue::UInt(0)
        );
        assert_eq!(
            eval(Op::CharOfCode, &[ConstValue::Int(65)], Ty::Char),
            ConstValue::Char(65)
        );
        assert!(evaluate(Op::CharOfCode, &[ConstValue::Int(0x1_0000)], Ty::Char).is_err());
        assert_eq!(
            eval(Op::ToString, &[ConstValue::UByte(255)], Ty::String),
            ConstValue::String(KtString::from("255"))
        );
    }

    #[test]
    fn the_feature_reads_the_operation_table_and_legacy_folding_needs_a_signed_kotlin_member() {
        let text = IntrinsicConstOwner::Package(crate::types::wk::kotlin_text_package());
        let declaration = |owner, name, receiver, first_parameter| IntrinsicConstDeclaration {
            owner,
            name,
            receiver,
            first_parameter,
        };
        let int_plus = declaration(
            IntrinsicConstOwner::Classifier(Ty::Int),
            "plus",
            Some(Ty::Int),
            Some(Ty::Int),
        );
        let uint_plus = declaration(
            IntrinsicConstOwner::Classifier(Ty::UInt),
            "plus",
            Some(Ty::UInt),
            Some(Ty::UInt),
        );
        let int_inc = declaration(
            IntrinsicConstOwner::Classifier(Ty::Int),
            "inc",
            Some(Ty::Int),
            None,
        );
        let trim = declaration(text, "trim", Some(Ty::String), None);
        let trim_boolean = declaration(text, "trim", Some(Ty::Boolean), None);
        for (declaration, legacy, with_feature) in [
            (int_plus, Some(Op::Plus), Some(Op::Plus)),
            (uint_plus, None, Some(Op::Plus)),
            (int_inc, None, Some(Op::Inc)),
            (trim, None, Some(Op::Trim)),
            (trim_boolean, None, None),
        ] {
            assert_eq!(
                compile_time_operation(declaration, false),
                legacy,
                "{declaration:?}"
            );
            assert_eq!(
                compile_time_operation(declaration, true),
                with_feature,
                "{declaration:?}"
            );
        }
    }
}
