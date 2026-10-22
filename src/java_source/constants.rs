//! Compile-time constants of Java source fields.
//!
//! A `static final` field of a primitive or `String` type whose initializer is a constant
//! expression is a constant variable (JLS §4.12.4), and kotlinc inlines a read of one. This
//! bounded header parser evaluates the initializers that are one literal, optionally signed: the
//! shape of the Android Gradle plugin's generated `BuildConfig` and of most hand-written constants.
//! A compound expression (`1 << 3`, `A + 1`, a cast) is no constant here, so its reads stay
//! `getstatic`.

use crate::ir::IrConst;
use crate::kt_string::KtString;

/// The value of a field initializer that is one literal, before the field's type converts it.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum JavaConstant {
    String(String),
    Boolean(bool),
    /// An `int` literal, already wrapped to its 32-bit value.
    Int(i32),
    Long(i64),
    Char(u16),
    Float(f32),
    Double(f64),
}

impl JavaConstant {
    /// The constant a field with JVM descriptor `descriptor` holds when initialized by this
    /// literal, under assignment conversion (JLS §5.2): widening, and the narrowing of an `int`
    /// constant to `byte`, `short` or `char` when it fits. `None` where javac rejects the
    /// initializer or the field is no constant variable's type.
    pub(crate) fn field_constant(&self, descriptor: &str) -> Option<IrConst> {
        use JavaConstant as C;
        let integral = match *self {
            C::Int(value) => Some(i64::from(value)),
            C::Char(value) => Some(i64::from(value)),
            _ => None,
        };
        Some(match (descriptor, self) {
            ("Ljava/lang/String;", C::String(value)) => {
                IrConst::String(KtString::from(value.clone()))
            }
            ("Z", C::Boolean(value)) => IrConst::Boolean(*value),
            ("I", _) => IrConst::Int(i32::try_from(integral?).ok()?),
            ("B", _) => IrConst::Byte(i8::try_from(integral?).ok()?),
            ("S", _) => IrConst::Short(i16::try_from(integral?).ok()?),
            ("C", _) => IrConst::Char(u16::try_from(integral?).ok()?),
            ("J", C::Long(value)) => IrConst::Long(*value),
            ("J", _) => IrConst::Long(integral?),
            ("F", C::Float(value)) => IrConst::Float(*value),
            ("F", C::Long(value)) => IrConst::Float(*value as f32),
            ("F", _) => IrConst::Float(integral? as f32),
            ("D", C::Double(value)) => IrConst::Double(*value),
            ("D", C::Float(value)) => IrConst::Double(f64::from(*value)),
            ("D", C::Long(value)) => IrConst::Double(*value as f64),
            ("D", _) => IrConst::Double(integral? as f64),
            _ => return None,
        })
    }
}

/// The literal spelled by `literal` (a lexed number, with its suffix) under `negative` sign, or
/// `None` for a malformed or out-of-range literal or a hexadecimal floating-point one.
pub(super) fn number_literal(literal: &str, negative: bool) -> Option<JavaConstant> {
    let lower = literal.replace('_', "").to_ascii_lowercase();
    let (radix, digits) = if let Some(rest) = lower.strip_prefix("0x") {
        if rest.contains('p') {
            return None;
        }
        (16, rest)
    } else if let Some(rest) = lower.strip_prefix("0b") {
        (2, rest)
    } else if lower.contains(['.', 'e']) || lower.ends_with(['f', 'd']) {
        return floating_literal(&lower, negative);
    } else {
        let body = lower.strip_suffix('l').unwrap_or(&lower);
        if body.len() > 1 && body.starts_with('0') {
            (8, &lower[1..])
        } else {
            (10, lower.as_str())
        }
    };
    let (digits, long) = match digits.strip_suffix('l') {
        Some(digits) => (digits, true),
        None => (digits, false),
    };
    let magnitude = u64::from_str_radix(digits, radix).ok()?;
    if radix == 10 {
        // A decimal literal states a magnitude, which only `-` may take to the type's minimum
        // (JLS §3.10.1).
        let limit = if long { 1u64 << 63 } else { 1u64 << 31 } - u64::from(!negative);
        if magnitude > limit {
            return None;
        }
        let value = if negative {
            -i128::from(magnitude)
        } else {
            i128::from(magnitude)
        };
        return Some(if long {
            JavaConstant::Long(i64::try_from(value).ok()?)
        } else {
            JavaConstant::Int(i32::try_from(value).ok()?)
        });
    }
    // The other radixes state the two's-complement bits of a value of the literal's own width, and
    // a unary `-` negates at that width: `-0x80000000` is still `Integer.MIN_VALUE`.
    Some(if long {
        let value = magnitude as i64;
        JavaConstant::Long(if negative {
            value.wrapping_neg()
        } else {
            value
        })
    } else {
        let value = u32::try_from(magnitude).ok()? as i32;
        JavaConstant::Int(if negative {
            value.wrapping_neg()
        } else {
            value
        })
    })
}

/// A decimal floating-point literal, rounded once to its own type (`f` for `float`, else `double`).
fn floating_literal(lower: &str, negative: bool) -> Option<JavaConstant> {
    let sign = if negative { "-" } else { "" };
    if let Some(value) = lower.strip_suffix('f') {
        format!("{sign}{value}")
            .parse::<f32>()
            .ok()
            .map(JavaConstant::Float)
    } else {
        let value = lower.strip_suffix('d').unwrap_or(lower);
        format!("{sign}{value}")
            .parse::<f64>()
            .ok()
            .map(JavaConstant::Double)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn integer_literals_follow_their_radix_and_sign() {
        assert_eq!(number_literal("42", false), Some(JavaConstant::Int(42)));
        assert_eq!(
            number_literal("2147483647", false),
            Some(JavaConstant::Int(i32::MAX))
        );
        assert_eq!(number_literal("2147483648", false), None);
        assert_eq!(
            number_literal("2147483648", true),
            Some(JavaConstant::Int(i32::MIN))
        );
        assert_eq!(
            number_literal("0x7f01_0000", false),
            Some(JavaConstant::Int(0x7f01_0000))
        );
        assert_eq!(
            number_literal("0xFFFFFFFF", false),
            Some(JavaConstant::Int(-1))
        );
        assert_eq!(number_literal("0b101", false), Some(JavaConstant::Int(5)));
        assert_eq!(number_literal("017", false), Some(JavaConstant::Int(15)));
        assert_eq!(number_literal("0", false), Some(JavaConstant::Int(0)));
        assert_eq!(number_literal("10L", true), Some(JavaConstant::Long(-10)));
        assert_eq!(
            number_literal("9223372036854775808L", true),
            Some(JavaConstant::Long(i64::MIN))
        );
        assert_eq!(number_literal("9223372036854775808L", false), None);
        assert_eq!(number_literal("0x1_0000_0000", false), None);
        assert_eq!(number_literal("08", false), None);
        assert_eq!(number_literal("0x", false), None);
    }

    #[test]
    fn zero_long_literals_are_decimal() {
        assert_eq!(number_literal("0L", false), Some(JavaConstant::Long(0)));
        assert_eq!(number_literal("0l", true), Some(JavaConstant::Long(0)));
        assert_eq!(number_literal("00L", false), Some(JavaConstant::Long(0)));
        assert_eq!(number_literal("017L", false), Some(JavaConstant::Long(15)));
    }

    #[test]
    fn non_decimal_negation_wraps_at_the_literal_width() {
        assert_eq!(
            number_literal("0x80000000", true),
            Some(JavaConstant::Int(i32::MIN))
        );
        assert_eq!(
            number_literal("020000000000", true),
            Some(JavaConstant::Int(i32::MIN))
        );
        assert_eq!(
            number_literal("0b1000_0000_0000_0000_0000_0000_0000_0000", true),
            Some(JavaConstant::Int(i32::MIN))
        );
        assert_eq!(
            number_literal("0xFFFFFFFF", true),
            Some(JavaConstant::Int(1))
        );
        assert_eq!(
            number_literal("0x8000000000000000L", true),
            Some(JavaConstant::Long(i64::MIN))
        );
    }

    #[test]
    fn floating_literals_round_once_to_their_own_type() {
        assert_eq!(
            number_literal("1.5f", false),
            Some(JavaConstant::Float(1.5))
        );
        assert_eq!(
            number_literal("0.1f", false),
            Some(JavaConstant::Float(0.1))
        );
        assert_eq!(
            number_literal("1e3", true),
            Some(JavaConstant::Double(-1000.0))
        );
        assert_eq!(number_literal(".5", false), Some(JavaConstant::Double(0.5)));
        assert_eq!(number_literal("2d", false), Some(JavaConstant::Double(2.0)));
        assert_eq!(number_literal("0x1.8p1", false), None);
    }

    #[test]
    fn assignment_conversion_types_the_literal_by_its_field() {
        let int = JavaConstant::Int(200);
        assert_eq!(int.field_constant("I"), Some(IrConst::Int(200)));
        assert_eq!(int.field_constant("J"), Some(IrConst::Long(200)));
        assert_eq!(int.field_constant("S"), Some(IrConst::Short(200)));
        assert_eq!(int.field_constant("B"), None);
        assert_eq!(int.field_constant("C"), Some(IrConst::Char(200)));
        assert_eq!(int.field_constant("D"), Some(IrConst::Double(200.0)));
        assert_eq!(int.field_constant("Z"), None);
        assert_eq!(
            JavaConstant::Boolean(false).field_constant("Z"),
            Some(IrConst::Boolean(false))
        );
        assert_eq!(
            JavaConstant::Char(u16::from(b'a')).field_constant("I"),
            Some(IrConst::Int(97))
        );
        assert_eq!(JavaConstant::Double(1.0).field_constant("F"), None);
        assert_eq!(JavaConstant::Long(1).field_constant("I"), None);
    }
}
