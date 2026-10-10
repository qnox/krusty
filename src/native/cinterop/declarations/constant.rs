//! Integer constant expressions (C11 §6.6), as array lengths, bitfield widths, enumerator values
//! and attribute arguments use them: C's integer types and conversions, `sizeof`, `_Alignof` and
//! `__builtin_offsetof` measured on the target, and casts.

use super::super::layout::enum_underlying;
use super::super::lexer::Kind;
use super::super::model::{IntRank, Origin, Signedness, Type, TypeId};
use super::super::preprocessor::{char_literal, integer_literal};
use super::parser::{Ordinary, Parser};
use super::ParseError;

/// An integer type, as constant arithmetic needs it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct IntType {
    rank: IntRank,
    signed: bool,
}

const INT: IntType = IntType {
    rank: IntRank::Int,
    signed: true,
};
const UNSIGNED_LONG: IntType = IntType {
    rank: IntRank::Long,
    signed: false,
};

impl IntType {
    fn bits(self) -> u32 {
        (self.rank.size() * 8) as u32
    }

    /// The integer promotions (C11 §6.3.1.1p2).
    fn promoted(self) -> Self {
        if self.rank < IntRank::Int {
            INT
        } else {
            self
        }
    }

    /// The usual arithmetic conversions' common type (C11 §6.3.1.8).
    fn common(self, other: Self) -> Self {
        let (left, right) = (self.promoted(), other.promoted());
        if left.signed == right.signed {
            return if left.rank >= right.rank { left } else { right };
        }
        let (unsigned, signed) = if left.signed {
            (right, left)
        } else {
            (left, right)
        };
        if unsigned.rank >= signed.rank {
            unsigned
        } else if signed.rank.size() > unsigned.rank.size() {
            signed
        } else {
            IntType {
                rank: signed.rank,
                signed: false,
            }
        }
    }

    /// `value` converted to this type, wrapping as C's conversions to unsigned types do (and as
    /// GCC and clang define the conversion to a signed type).
    fn wrap(self, value: i128) -> i128 {
        if self.rank == IntRank::Bool {
            return i128::from(value != 0);
        }
        let bits = self.bits();
        if bits >= 128 {
            return value;
        }
        let modulus = 1i128 << bits;
        let wrapped = value.rem_euclid(modulus);
        if self.signed && wrapped >= modulus / 2 {
            wrapped - modulus
        } else {
            wrapped
        }
    }

    fn holds(self, value: i128) -> bool {
        self.wrap(value) == value
    }
}

/// The value of an integer constant expression, with its type.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Value {
    pub(super) value: i128,
    ty: IntType,
}

impl Value {
    fn new(value: i128, ty: IntType) -> Self {
        Self {
            value: ty.wrap(value),
            ty,
        }
    }

    fn int(value: bool) -> Self {
        Self::new(i128::from(value), INT)
    }

    /// An enumeration constant: an `int` when its value is one, else the smallest type that
    /// holds it, as GCC and clang extend C11 §6.7.2.2p2.
    pub(super) fn enumerator(value: i128) -> Self {
        let ty = [
            INT,
            IntType {
                rank: IntRank::Int,
                signed: false,
            },
            IntType {
                rank: IntRank::Long,
                signed: true,
            },
            UNSIGNED_LONG,
        ]
        .into_iter()
        .find(|ty| ty.holds(value))
        .unwrap_or(IntType {
            rank: IntRank::Int128,
            signed: true,
        });
        Self::new(value, ty)
    }

    pub(super) fn is_zero(self) -> bool {
        self.value == 0
    }

    pub(super) fn is_negative(self) -> bool {
        self.value < 0
    }

    /// The value as an unsigned size; callers reject a negative one first where it matters.
    pub(super) fn as_u64(self) -> u64 {
        self.value as u64
    }

    fn converted(self, ty: IntType) -> i128 {
        ty.wrap(self.value)
    }
}

/// Binary operators by precedence, lowest first.
fn precedence(operator: &str) -> Option<u8> {
    Some(match operator {
        "||" => 0,
        "&&" => 1,
        "|" => 2,
        "^" => 3,
        "&" => 4,
        "==" | "!=" => 5,
        "<" | ">" | "<=" | ">=" => 6,
        "<<" | ">>" => 7,
        "+" | "-" => 8,
        "*" | "/" | "%" => 9,
        _ => return None,
    })
}

impl Parser<'_> {
    /// A conditional expression that must be an integer constant.
    pub(super) fn constant_expression(&mut self) -> Result<Value, ParseError> {
        let condition = self.binary(0)?;
        if !self.eat("?") {
            return Ok(condition);
        }
        let then = self.comma_expression()?;
        self.expect(":")?;
        let otherwise = self.constant_expression()?;
        let ty = then.ty.common(otherwise.ty);
        let chosen = if condition.is_zero() { otherwise } else { then };
        Ok(Value::new(chosen.converted(ty), ty))
    }

    fn comma_expression(&mut self) -> Result<Value, ParseError> {
        let mut value = self.constant_expression()?;
        while self.eat(",") {
            value = self.constant_expression()?;
        }
        Ok(value)
    }

    fn binary(&mut self, min_precedence: u8) -> Result<Value, ParseError> {
        let mut left = self.cast_expression()?;
        loop {
            let Some(operator) = self
                .peek()
                .filter(|token| token.kind == Kind::Punctuator)
                .and_then(|token| precedence(&token.text).map(|level| (token, level)))
                .filter(|(_, level)| *level >= min_precedence)
            else {
                return Ok(left);
            };
            let (token, level) = operator;
            let origin = self.origin();
            self.next();
            let right = self.binary(level + 1)?;
            left = self.apply(&token.text, left, right, &origin)?;
        }
    }

    fn apply(
        &self,
        operator: &str,
        left: Value,
        right: Value,
        origin: &Origin,
    ) -> Result<Value, ParseError> {
        match operator {
            "&&" => return Ok(Value::int(!left.is_zero() && !right.is_zero())),
            "||" => return Ok(Value::int(!left.is_zero() || !right.is_zero())),
            "<<" | ">>" => {
                let ty = left.ty.promoted();
                let shift = right.value;
                if shift < 0 || shift >= i128::from(ty.bits()) {
                    return Err(self.error_at(origin, "shift count is out of range"));
                }
                let value = left.converted(ty);
                let shifted = if operator == "<<" {
                    value << shift
                } else {
                    value >> shift
                };
                return Ok(Value::new(shifted, ty));
            }
            _ => {}
        }
        let ty = left.ty.common(right.ty);
        let (l, r) = (left.converted(ty), right.converted(ty));
        let value = match operator {
            "*" => l.wrapping_mul(r),
            "/" | "%" => {
                if r == 0 {
                    return Err(self.error_at(origin, "division by zero"));
                }
                if operator == "/" {
                    l / r
                } else {
                    l % r
                }
            }
            "+" => l.wrapping_add(r),
            "-" => l.wrapping_sub(r),
            "&" => l & r,
            "^" => l ^ r,
            "|" => l | r,
            "==" => return Ok(Value::int(l == r)),
            "!=" => return Ok(Value::int(l != r)),
            "<" => return Ok(Value::int(l < r)),
            ">" => return Ok(Value::int(l > r)),
            "<=" => return Ok(Value::int(l <= r)),
            ">=" => return Ok(Value::int(l >= r)),
            other => unreachable!("`{other}` has a precedence but no meaning"),
        };
        Ok(Value::new(value, ty))
    }

    fn cast_expression(&mut self) -> Result<Value, ParseError> {
        if self.at("(")
            && self
                .peek_at(1)
                .is_some_and(|next| self.begins_specifiers(next))
        {
            self.next();
            let origin = self.origin();
            let ty = self.type_name()?;
            self.expect(")")?;
            let value = self.cast_expression()?;
            let target = self.integer_type(ty, &origin)?;
            return Ok(Value::new(value.value, target));
        }
        self.unary_expression()
    }

    fn unary_expression(&mut self) -> Result<Value, ParseError> {
        let Some(token) = self.peek() else {
            return Err(self.error("expected an expression"));
        };
        let origin = self.origin();
        match &*token.text {
            "-" | "+" | "~" | "!" if token.kind == Kind::Punctuator => {
                self.next();
                let operand = self.cast_expression()?;
                let ty = operand.ty.promoted();
                let value = operand.converted(ty);
                Ok(match &*token.text {
                    "-" => Value::new(value.wrapping_neg(), ty),
                    "+" => Value::new(value, ty),
                    "~" => Value::new(!value, ty),
                    _ => Value::int(value == 0),
                })
            }
            "__extension__" => {
                self.next();
                self.cast_expression()
            }
            "sizeof" => {
                self.next();
                let ty = self.measured_operand()?;
                let layout = self
                    .layouts
                    .layout(&self.declarations, ty)
                    .map_err(|error| self.layout_error(&origin, error))?;
                Ok(Value::new(i128::from(layout.size), UNSIGNED_LONG))
            }
            "_Alignof" | "__alignof__" | "__alignof" | "alignof" => {
                self.next();
                let ty = self.measured_operand()?;
                let layout = self
                    .layouts
                    .layout(&self.declarations, ty)
                    .map_err(|error| self.layout_error(&origin, error))?;
                Ok(Value::new(i128::from(layout.align), UNSIGNED_LONG))
            }
            "__builtin_offsetof" => self.offsetof(),
            _ => self.primary_expression(),
        }
    }

    /// The operand of `sizeof` or `_Alignof`: a parenthesized type name or an expression.
    fn measured_operand(&mut self) -> Result<TypeId, ParseError> {
        if self.at("(")
            && self
                .peek_at(1)
                .is_some_and(|next| self.begins_specifiers(next))
        {
            self.next();
            let ty = self.type_name()?;
            self.expect(")")?;
            return Ok(ty);
        }
        self.expression_type()
    }

    fn primary_expression(&mut self) -> Result<Value, ParseError> {
        let token = self.peek().expect("checked by the caller");
        match token.kind {
            Kind::Number => {
                self.next();
                self.integer_constant(&token.text)
            }
            Kind::CharLiteral => {
                self.next();
                let value = char_literal(&token.text).ok_or_else(|| {
                    self.error(&format!("invalid character constant {}", token.text))
                })?;
                // An unprefixed constant is an `int` holding a `char`'s value.
                let value = if token.text.starts_with('\'')
                    && self.layouts.char_is_signed()
                    && (128..256).contains(&value)
                {
                    i128::from(value) - 256
                } else {
                    i128::from(value)
                };
                Ok(Value::new(value, INT))
            }
            Kind::Punctuator if token.is("(") => {
                self.next();
                let value = self.comma_expression()?;
                self.expect(")")?;
                Ok(value)
            }
            Kind::Identifier => match self.ordinary.get(&token.text) {
                Some(Ordinary::Constant(value)) => {
                    let value = *value;
                    self.next();
                    Ok(value)
                }
                _ => Err(self.error(&format!("'{}' is not an integer constant", token.text))),
            },
            _ => Err(self.error(&format!(
                "expected an integer constant expression{}",
                self.found()
            ))),
        }
    }

    /// An integer constant's value and type (C11 §6.4.4.1p5): the first of its suffix's candidate
    /// types that holds it.
    fn integer_constant(&self, text: &str) -> Result<Value, ParseError> {
        let lower = text.to_ascii_lowercase();
        let hex = lower.starts_with("0x");
        if lower.contains('.') || (!hex && lower.contains('e')) || (hex && lower.contains('p')) {
            return Err(self.error(&format!(
                "floating constant {text} in an integer constant expression"
            )));
        }
        let (magnitude, _) = integer_literal(text)
            .ok_or_else(|| self.error(&format!("invalid integer constant {text}")))?;
        let suffix_start = lower.trim_end_matches(['u', 'l']).len();
        let suffix = &lower[suffix_start..];
        let unsigned = suffix.contains('u');
        let longs = suffix.matches('l').count();
        let decimal = !lower.starts_with('0') || lower.len() == 1;
        let ty = |rank, signed| IntType { rank, signed };
        let mut candidates = Vec::new();
        for rank in [IntRank::Int, IntRank::Long, IntRank::LongLong] {
            let wanted = match rank {
                IntRank::Int => 0,
                IntRank::Long => 1,
                _ => 2,
            };
            if wanted < longs {
                continue;
            }
            if !unsigned {
                candidates.push(ty(rank, true));
            }
            if unsigned || !decimal {
                candidates.push(ty(rank, false));
            }
        }
        let value = i128::from(magnitude);
        let ty = candidates
            .into_iter()
            .find(|candidate| candidate.holds(value))
            .unwrap_or(ty(IntRank::LongLong, false));
        Ok(Value::new(value, ty))
    }

    /// The integer type a cast to `ty` converts to.
    fn integer_type(&self, ty: TypeId, origin: &Origin) -> Result<IntType, ParseError> {
        let canonical = self.declarations.canonical(ty);
        Ok(match self.declarations.ty(canonical) {
            Type::Int { rank, signedness } => IntType {
                rank: *rank,
                signed: match signedness {
                    Signedness::Signed => true,
                    Signedness::Unsigned => false,
                    Signedness::Plain => self.layouts.char_is_signed(),
                },
            },
            Type::Enum(id) => {
                let Some((rank, signedness)) = enum_underlying(self.declarations.enumeration(*id))
                else {
                    return Err(self.error_at(origin, "cast to an incomplete enum"));
                };
                IntType {
                    rank,
                    signed: signedness == Signedness::Signed,
                }
            }
            Type::Pointer(_) => UNSIGNED_LONG,
            _ => {
                return Err(self.error_at(
                    origin,
                    &format!(
                        "cast to '{}' in an integer constant expression",
                        self.declarations.spell(ty)
                    ),
                ));
            }
        })
    }

    /// `__builtin_offsetof ( type-name , member-designator )`, in bytes.
    fn offsetof(&mut self) -> Result<Value, ParseError> {
        let origin = self.origin();
        self.next();
        self.expect("(")?;
        let mut current = self.type_name()?;
        self.expect(",")?;
        let mut offset_bits = 0u64;
        let mut member = true;
        loop {
            if member {
                let name = self.identifier()?.text.clone();
                let Type::Record(record) =
                    *self.declarations.ty(self.declarations.canonical(current))
                else {
                    return Err(self.error_at(&origin, "offsetof needs a struct or union"));
                };
                let Some(path) = self.declarations.member_path(record, &name) else {
                    return Err(self.error_at(&origin, &format!("no member named '{name}'")));
                };
                let mut record = record;
                for index in path {
                    let layout = self
                        .layouts
                        .record(&self.declarations, record)
                        .map_err(|error| self.layout_error(&origin, error))?;
                    let fields = self.declarations.record(record).fields.as_ref();
                    let field = &fields.expect("laid out, so defined")[index];
                    if field.bit_width.is_some() {
                        return Err(self.error_at(&origin, "offsetof of a bitfield"));
                    }
                    offset_bits += layout.field_offsets[index];
                    current = field.ty;
                    if let Type::Record(inner) =
                        *self.declarations.ty(self.declarations.canonical(current))
                    {
                        record = inner;
                    }
                }
            }
            if self.eat(".") {
                member = true;
            } else if self.eat("[") {
                let index = self.comma_expression()?;
                self.expect("]")?;
                let Type::Array { element, .. } =
                    *self.declarations.ty(self.declarations.canonical(current))
                else {
                    return Err(self.error_at(&origin, "offsetof subscripts a non-array"));
                };
                let size = self
                    .layouts
                    .layout(&self.declarations, element)
                    .map_err(|error| self.layout_error(&origin, error))?
                    .size;
                offset_bits = offset_bits.wrapping_add(index.as_u64().wrapping_mul(size * 8));
                current = element;
                member = false;
            } else {
                break;
            }
        }
        self.expect(")")?;
        Ok(Value::new(i128::from(offset_bits / 8), UNSIGNED_LONG))
    }

    /// The type of an expression operand of `sizeof`, `_Alignof` or `__typeof__`: a constant, a
    /// string, a declared object, or one reached through `*`, in parentheses or not.
    pub(super) fn expression_type(&mut self) -> Result<TypeId, ParseError> {
        let Some(token) = self.peek() else {
            return Err(self.error("expected an expression"));
        };
        match token.kind {
            Kind::Punctuator if token.is("(") => {
                self.next();
                let ty = self.expression_type()?;
                self.expect(")")?;
                Ok(ty)
            }
            Kind::Punctuator if token.is("*") => {
                self.next();
                let origin = self.origin();
                let pointer = self.expression_type()?;
                match *self.declarations.ty(self.declarations.canonical(pointer)) {
                    Type::Pointer(pointee) => Ok(pointee),
                    Type::Array { element, .. } => Ok(element),
                    _ => Err(self.error_at(&origin, "indirection requires a pointer operand")),
                }
            }
            Kind::StringLiteral => {
                let mut length = 0u64;
                while let Some(token) = self
                    .peek()
                    .filter(|token| token.kind == Kind::StringLiteral)
                {
                    let Some(body) = token
                        .text
                        .strip_prefix('"')
                        .and_then(|t| t.strip_suffix('"'))
                    else {
                        return Err(self.error("only plain string literals can be measured"));
                    };
                    if body.contains('\\') {
                        return Err(self.error("a string with escapes cannot be measured"));
                    }
                    length += body.len() as u64;
                    self.next();
                }
                let char_type = self.int_type(IntRank::Char, Signedness::Plain);
                Ok(self.declarations.intern(Type::Array {
                    element: char_type,
                    length: Some(length + 1),
                }))
            }
            Kind::Identifier => match self.ordinary.get(&token.text).copied() {
                Some(Ordinary::Object(ty)) => {
                    self.next();
                    Ok(ty)
                }
                Some(Ordinary::Constant(_)) | None => {
                    let value = self.constant_expression()?;
                    Ok(self.int_type(
                        value.ty.rank,
                        if value.ty.signed {
                            Signedness::Signed
                        } else {
                            Signedness::Unsigned
                        },
                    ))
                }
                Some(Ordinary::Typedef(_)) => {
                    Err(self.error(&format!("unexpected type name '{}'", token.text)))
                }
            },
            _ => {
                let value = self.constant_expression()?;
                Ok(self.int_type(
                    value.ty.rank,
                    if value.ty.signed {
                        Signedness::Signed
                    } else {
                        Signedness::Unsigned
                    },
                ))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_usual_arithmetic_conversions_pick_cs_common_type() {
        let unsigned_int = IntType {
            rank: IntRank::Int,
            signed: false,
        };
        let long = IntType {
            rank: IntRank::Long,
            signed: true,
        };
        let long_long = IntType {
            rank: IntRank::LongLong,
            signed: true,
        };
        let char_type = IntType {
            rank: IntRank::Char,
            signed: false,
        };
        assert_eq!(char_type.common(char_type), INT);
        assert_eq!(INT.common(unsigned_int), unsigned_int);
        assert_eq!(unsigned_int.common(long), long);
        assert_eq!(
            UNSIGNED_LONG.common(long_long),
            IntType {
                rank: IntRank::LongLong,
                signed: false,
            }
        );
        assert_eq!(unsigned_int.wrap(-1), 0xffff_ffff);
        assert_eq!(INT.wrap(0x8000_0000), -0x8000_0000);
        assert_eq!(Value::enumerator(0x8000_0000).ty, unsigned_int);
    }
}
