//! The value of an object-like macro, read as cinterop reads one: the macro's expansion, after the
//! whole header, as a C expression whose type and value a C compiler would give it.
//!
//! An integer or floating constant expression has a value and a type; the type is the
//! expression's own, so a cast keeps the typedef it names (`((uint) 3)` is a `uint`). Adjacent
//! plain or `u8` string literals are a string. Any other expression that is well typed — one
//! reading a variable, a parenthesized string — has a type but no value. Anything else (a type
//! name, a statement, an empty body, a wide string) has neither, and is not bound.

use std::rc::Rc;

use super::super::lexer::{Kind, Token};
use super::super::model::{FloatKind, IntRank, MacroValue, Signedness, Type, TypeId};
use super::super::preprocessor::char_literal;
use super::constant::{precedence, IntType, Value};
use super::parser::{Ordinary, Parser};

/// A number an expression evaluated to.
#[derive(Clone, Copy, Debug)]
enum Number {
    Int(Value),
    Float(f64),
}

/// An expression's type, and its value when it is a constant.
#[derive(Clone, Copy, Debug)]
struct Operand {
    ty: TypeId,
    number: Option<Number>,
}

/// What an arithmetic type is, for the usual arithmetic conversions.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Arithmetic {
    Int(IntType),
    Float(FloatKind),
}

impl<'t> Parser<'t> {
    /// The value of a macro whose expansion is `tokens`; `None` when it is not bound.
    pub(super) fn macro_value(&mut self, tokens: &'t [Token]) -> Option<MacroValue> {
        self.reading(tokens, Self::macro_body)
    }

    fn macro_body(&mut self) -> Option<MacroValue> {
        let first = self.peek()?;
        if first.kind == Kind::StringLiteral {
            let mut text = Vec::new();
            while let Some(token) = self
                .peek()
                .filter(|token| token.kind == Kind::StringLiteral)
            {
                text.extend(string_literal(&token.text)?);
                self.next();
            }
            if self.peek().is_some() {
                return None;
            }
            return Some(MacroValue::String(Rc::from(
                String::from_utf8_lossy(&text).as_ref(),
            )));
        }
        let operand = self.conditional(true)?;
        if self.peek().is_some() {
            return None;
        }
        Some(match operand.number {
            Some(Number::Int(value)) => MacroValue::Integer {
                value: value.value,
                ty: operand.ty,
            },
            Some(Number::Float(value)) => MacroValue::Floating {
                value,
                ty: operand.ty,
            },
            None => MacroValue::Expression(operand.ty),
        })
    }

    fn conditional(&mut self, evaluate: bool) -> Option<Operand> {
        let condition = self.binary_operand(0, evaluate)?;
        if !self.eat("?") {
            return Some(condition);
        }
        let condition_is_zero = condition.number.map(is_zero);
        let then = self.conditional(evaluate && condition_is_zero == Some(false))?;
        if !self.eat(":") {
            return None;
        }
        let otherwise = self.conditional(evaluate && condition_is_zero == Some(true))?;
        let (ty, kind) = self.common_type(then.ty, otherwise.ty)?;
        let chosen = match (evaluate, condition.number) {
            (false, _) => return Some(Operand { ty, number: None }),
            (true, Some(number)) if is_zero(number) => otherwise,
            (true, Some(_)) => then,
            (true, None) => return Some(Operand { ty, number: None }),
        };
        Some(Operand {
            ty,
            number: chosen.number.map(|number| convert(number, kind)),
        })
    }

    fn binary_operand(&mut self, min_precedence: u8, evaluate: bool) -> Option<Operand> {
        let mut left = self.cast_operand(evaluate)?;
        loop {
            let Some((operator, level)) = self
                .peek()
                .filter(|token| token.kind == Kind::Punctuator)
                .and_then(|token| precedence(&token.text).map(|level| (token, level)))
                .filter(|(_, level)| *level >= min_precedence)
            else {
                return Some(left);
            };
            let operator = operator.text.clone();
            self.next();
            let evaluate_right = evaluate
                && match (&*operator, left.number) {
                    ("&&", Some(number)) => !is_zero(number),
                    ("||", Some(number)) => is_zero(number),
                    ("&&" | "||", None) => false,
                    _ => true,
                };
            let right = self.binary_operand(level + 1, evaluate_right)?;
            left = self.binary_operation(&operator, left, right, evaluate)?;
        }
    }

    fn binary_operation(
        &mut self,
        operator: &str,
        left: Operand,
        right: Operand,
        evaluate: bool,
    ) -> Option<Operand> {
        let int = self.int_type(IntRank::Int, Signedness::Signed);
        let comparison = matches!(
            operator,
            "==" | "!=" | "<" | ">" | "<=" | ">=" | "&&" | "||"
        );
        let (left_kind, right_kind) = (self.arithmetic(left.ty), self.arithmetic(right.ty));
        let (Some(left_kind), Some(right_kind)) = (left_kind, right_kind) else {
            // Pointer arithmetic and comparison: never a constant cinterop binds.
            let ty = match operator {
                _ if comparison => int,
                "+" | "-" if left_kind.is_none() && right_kind.is_some() => left.ty,
                "+" if right_kind.is_none() && left_kind.is_some() => right.ty,
                "-" if left_kind.is_none() && right_kind.is_none() => {
                    self.int_type(IntRank::Long, Signedness::Signed)
                }
                _ => return None,
            };
            return Some(Operand { ty, number: None });
        };
        let floating =
            matches!(left_kind, Arithmetic::Float(_)) || matches!(right_kind, Arithmetic::Float(_));
        if floating && matches!(operator, "%" | "<<" | ">>" | "&" | "|" | "^") {
            return None;
        }
        let ty = if comparison {
            int
        } else if matches!(operator, "<<" | ">>") {
            let Arithmetic::Int(left_int) = left_kind else {
                return None;
            };
            self.int_type_id(left_int.promoted())
        } else {
            self.common_type(left.ty, right.ty)?.0
        };
        if !evaluate {
            return Some(Operand { ty, number: None });
        }
        if let Some(left_number) = left.number {
            let short_circuit = match operator {
                "&&" if is_zero(left_number) => Some(false),
                "||" if !is_zero(left_number) => Some(true),
                _ => None,
            };
            if let Some(value) = short_circuit {
                return Some(Operand {
                    ty,
                    number: Some(Number::Int(Value::new(i128::from(value), int_type()))),
                });
            }
        }
        let (Some(left_number), Some(right_number)) = (left.number, right.number) else {
            return Some(Operand { ty, number: None });
        };
        let number = if floating {
            let (l, r) = (as_float(left_number), as_float(right_number));
            let truth = |value: bool| Number::Int(Value::new(i128::from(value), int_type()));
            match operator {
                "+" => Number::Float(l + r),
                "-" => Number::Float(l - r),
                "*" => Number::Float(l * r),
                "/" => Number::Float(l / r),
                "==" => truth(l == r),
                "!=" => truth(l != r),
                "<" => truth(l < r),
                ">" => truth(l > r),
                "<=" => truth(l <= r),
                ">=" => truth(l >= r),
                "&&" => truth(l != 0.0 && r != 0.0),
                "||" => truth(l != 0.0 || r != 0.0),
                _ => return None,
            }
        } else {
            let (Number::Int(l), Number::Int(r)) = (left_number, right_number) else {
                return None;
            };
            let origin = self.origin();
            Number::Int(self.apply(operator, l, r, &origin, true).ok()?)
        };
        let number = match self.arithmetic(ty) {
            Some(Arithmetic::Float(kind)) => convert(number, Arithmetic::Float(kind)),
            _ => number,
        };
        Some(Operand {
            ty,
            number: Some(number),
        })
    }

    fn cast_operand(&mut self, evaluate: bool) -> Option<Operand> {
        if self.at("(")
            && self
                .peek_at(1)
                .is_some_and(|next| self.begins_specifiers(next))
        {
            self.next();
            let ty = self.type_name().ok()?;
            if !self.eat(")") {
                return None;
            }
            let operand = self.cast_operand(evaluate)?;
            let number = match (evaluate, self.arithmetic(ty), operand.number) {
                (false, _, _) => None,
                (true, Some(kind), Some(number)) => Some(convert(number, kind)),
                _ => None,
            };
            if matches!(
                self.declarations.ty(self.declarations.canonical(ty)),
                Type::Void
            ) {
                return None;
            }
            return Some(Operand { ty, number });
        }
        self.unary_operand(evaluate)
    }

    fn unary_operand(&mut self, evaluate: bool) -> Option<Operand> {
        let token = self.peek()?;
        let int = self.int_type(IntRank::Int, Signedness::Signed);
        match &*token.text {
            "-" | "+" | "~" | "!" if token.kind == Kind::Punctuator => {
                self.next();
                let operand = self.cast_operand(evaluate)?;
                let kind = self.arithmetic(operand.ty)?;
                let ty = match (&*token.text, kind) {
                    ("!", _) => int,
                    (_, Arithmetic::Int(int_type)) => self.int_type_id(int_type.promoted()),
                    ("~", Arithmetic::Float(_)) => return None,
                    (_, Arithmetic::Float(_)) => operand.ty,
                };
                let number = evaluate.then_some(operand.number).flatten().map(|number| {
                    match (&*token.text, number) {
                        ("!", number) => {
                            Number::Int(Value::new(i128::from(is_zero(number)), int_type()))
                        }
                        ("-", Number::Float(value)) => Number::Float(-value),
                        (_, Number::Float(value)) => Number::Float(value),
                        (operator, Number::Int(value)) => {
                            let promoted = value.ty().promoted();
                            let converted = value.converted(promoted);
                            Number::Int(Value::new(
                                match operator {
                                    "-" => converted.wrapping_neg(),
                                    "~" => !converted,
                                    _ => converted,
                                },
                                promoted,
                            ))
                        }
                    }
                });
                Some(Operand { ty, number })
            }
            "*" if token.kind == Kind::Punctuator => {
                self.next();
                let operand = self.cast_operand(evaluate)?;
                let pointee = match *self
                    .declarations
                    .ty(self.declarations.canonical(operand.ty))
                {
                    Type::Pointer(pointee) => pointee,
                    Type::Array { element, .. } => element,
                    _ => return None,
                };
                Some(Operand {
                    ty: pointee,
                    number: None,
                })
            }
            "&" if token.kind == Kind::Punctuator => {
                self.next();
                let operand = self.cast_operand(evaluate)?;
                Some(Operand {
                    ty: self.declarations.intern(Type::Pointer(operand.ty)),
                    number: None,
                })
            }
            "__extension__" => {
                self.next();
                self.cast_operand(evaluate)
            }
            "sizeof" | "_Alignof" | "__alignof__" | "__alignof" | "alignof" => {
                let size = token.is("sizeof");
                self.next();
                let measured = self.measured_operand().ok()?;
                let layout = self.layouts.layout(&self.declarations, measured).ok()?;
                let unsigned_long = int_type_of(IntRank::Long, false);
                Some(Operand {
                    ty: self.int_type_id(unsigned_long),
                    number: evaluate.then(|| {
                        Number::Int(Value::new(
                            i128::from(if size { layout.size } else { layout.align }),
                            unsigned_long,
                        ))
                    }),
                })
            }
            _ => {
                let operand = self.primary_operand(evaluate)?;
                self.postfix(operand)
            }
        }
    }

    /// Calls, subscripts and member accesses after an operand: none of them is a constant.
    fn postfix(&mut self, mut operand: Operand) -> Option<Operand> {
        loop {
            let canonical = self.declarations.canonical(operand.ty);
            if self.at("(") {
                let function = match *self.declarations.ty(canonical) {
                    Type::Function(_) => canonical,
                    Type::Pointer(pointee) => self.declarations.canonical(pointee),
                    _ => return None,
                };
                let Type::Function(function) = self.declarations.ty(function) else {
                    return None;
                };
                let result = function.result;
                self.skip_group().ok()?;
                operand = Operand {
                    ty: result,
                    number: None,
                };
            } else if self.at("[") {
                let element = match *self.declarations.ty(canonical) {
                    Type::Pointer(element) | Type::Array { element, .. } => element,
                    _ => return None,
                };
                self.skip_group().ok()?;
                operand = Operand {
                    ty: element,
                    number: None,
                };
            } else if self.at_any(&[".", "->"]) {
                let arrow = self.at("->");
                self.next();
                let record = if arrow {
                    match *self.declarations.ty(canonical) {
                        Type::Pointer(pointee) => self.declarations.canonical(pointee),
                        _ => return None,
                    }
                } else {
                    canonical
                };
                let Type::Record(mut record) = *self.declarations.ty(record) else {
                    return None;
                };
                let name = self.identifier().ok()?.text.clone();
                let path = self.declarations.member_path(record, &name)?;
                let mut ty = operand.ty;
                for index in path {
                    let fields = self.declarations.record(record).fields.as_ref()?;
                    ty = fields[index].ty;
                    if let Type::Record(inner) =
                        *self.declarations.ty(self.declarations.canonical(ty))
                    {
                        record = inner;
                    }
                }
                operand = Operand { ty, number: None };
            } else {
                return Some(operand);
            }
        }
    }

    fn primary_operand(&mut self, evaluate: bool) -> Option<Operand> {
        let token = self.peek()?;
        match token.kind {
            Kind::Number => {
                self.next();
                if is_floating_literal(&token.text) {
                    let (value, kind) = floating_literal(&token.text)?;
                    return Some(Operand {
                        ty: self.declarations.intern(Type::Float(kind)),
                        number: evaluate.then_some(Number::Float(value)),
                    });
                }
                let value = self.integer_constant(&token.text).ok()?;
                Some(Operand {
                    ty: self.value_type(value),
                    number: evaluate.then_some(Number::Int(value)),
                })
            }
            Kind::CharLiteral => {
                self.next();
                if !token.text.starts_with('\'') {
                    return None;
                }
                let value = i128::from(char_literal(&token.text)?);
                let value = if self.layouts.char_is_signed() && (128..256).contains(&value) {
                    value - 256
                } else {
                    value
                };
                Some(Operand {
                    ty: self.int_type(IntRank::Int, Signedness::Signed),
                    number: evaluate.then_some(Number::Int(Value::new(value, int_type()))),
                })
            }
            Kind::StringLiteral => {
                let mut length = 0u64;
                while let Some(token) = self
                    .peek()
                    .filter(|token| token.kind == Kind::StringLiteral)
                {
                    length += string_literal(&token.text)?.len() as u64;
                    self.next();
                }
                let char_type = self.int_type(IntRank::Char, Signedness::Plain);
                Some(Operand {
                    ty: self.declarations.intern(Type::Array {
                        element: char_type,
                        length: Some(length + 1),
                    }),
                    number: None,
                })
            }
            Kind::Punctuator if token.is("(") => {
                self.next();
                let mut operand = self.conditional(evaluate)?;
                while self.eat(",") {
                    operand = self.conditional(evaluate)?;
                    operand.number = None;
                }
                self.eat(")").then_some(operand)
            }
            Kind::Identifier => {
                self.next();
                match self.ordinary.get(&token.text).copied()? {
                    Ordinary::Constant(value) => Some(Operand {
                        ty: self.int_type(IntRank::Int, Signedness::Signed),
                        number: evaluate.then_some(Number::Int(value)),
                    }),
                    Ordinary::Object(ty) => Some(Operand { ty, number: None }),
                    Ordinary::Typedef(_) => None,
                }
            }
            _ => None,
        }
    }

    /// What arithmetic `ty` takes part in; `None` for a pointer, record or other type.
    fn arithmetic(&self, ty: TypeId) -> Option<Arithmetic> {
        let canonical = self.declarations.canonical(ty);
        match self.declarations.ty(canonical) {
            Type::Float(kind) => Some(Arithmetic::Float(*kind)),
            Type::Int { .. } | Type::Enum(_) => self
                .integer_type(canonical, &self.origin())
                .ok()
                .map(Arithmetic::Int),
            _ => None,
        }
    }

    /// The usual arithmetic conversions' common type of two operands' types.
    fn common_type(&mut self, left: TypeId, right: TypeId) -> Option<(TypeId, Arithmetic)> {
        let (left_kind, right_kind) = (self.arithmetic(left)?, self.arithmetic(right)?);
        let kind = match (left_kind, right_kind) {
            (Arithmetic::Float(l), Arithmetic::Float(r)) => Arithmetic::Float(l.max(r)),
            (Arithmetic::Float(kind), _) | (_, Arithmetic::Float(kind)) => Arithmetic::Float(kind),
            (Arithmetic::Int(l), Arithmetic::Int(r)) => Arithmetic::Int(l.common(r)),
        };
        let ty = match kind {
            Arithmetic::Float(kind) if Arithmetic::Float(kind) == left_kind => left,
            Arithmetic::Float(kind) if Arithmetic::Float(kind) == right_kind => right,
            Arithmetic::Float(kind) => self.declarations.intern(Type::Float(kind)),
            Arithmetic::Int(int_type) => self.int_type_id(int_type),
        };
        Some((ty, kind))
    }
}

fn int_type_of(rank: IntRank, signed: bool) -> IntType {
    IntType::new(rank, signed)
}

fn int_type() -> IntType {
    int_type_of(IntRank::Int, true)
}

fn is_zero(number: Number) -> bool {
    match number {
        Number::Int(value) => value.is_zero(),
        Number::Float(value) => value == 0.0,
    }
}

fn as_float(number: Number) -> f64 {
    match number {
        Number::Int(value) => value.value as f64,
        Number::Float(value) => value,
    }
}

/// `number` converted to a value of `kind`.
fn convert(number: Number, kind: Arithmetic) -> Number {
    match (kind, number) {
        (Arithmetic::Int(ty), Number::Int(value)) => Number::Int(Value::new(value.value, ty)),
        (Arithmetic::Int(ty), Number::Float(value)) => {
            Number::Int(Value::new(value.trunc() as i128, ty))
        }
        (Arithmetic::Float(FloatKind::Float), number) => {
            Number::Float(f64::from(as_float(number) as f32))
        }
        (Arithmetic::Float(_), number) => Number::Float(as_float(number)),
    }
}

fn is_floating_literal(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    if lower.starts_with("0x") {
        lower.contains('p') || lower.contains('.')
    } else {
        lower.contains('.') || lower.contains('e')
    }
}

/// A decimal floating constant's value and type: `f` makes it a `float`, `l` a `long double`.
fn floating_literal(text: &str) -> Option<(f64, FloatKind)> {
    let lower = text.to_ascii_lowercase();
    let (digits, kind) = match lower.strip_suffix('f') {
        Some(digits) if !lower.starts_with("0x") || lower.contains('p') => {
            (digits, FloatKind::Float)
        }
        _ => match lower.strip_suffix('l') {
            Some(digits) => (digits, FloatKind::LongDouble),
            None => (lower.as_str(), FloatKind::Double),
        },
    };
    if digits.starts_with("0x") {
        return None;
    }
    let value: f64 = digits.parse().ok()?;
    Some(match kind {
        FloatKind::Float => (f64::from(value as f32), kind),
        _ => (value, kind),
    })
}

/// The bytes a plain or `u8` string literal spells; `None` for a wide one.
fn string_literal(text: &str) -> Option<Vec<u8>> {
    let body = text.strip_prefix("u8").unwrap_or(text);
    let body = body.strip_prefix('"')?.strip_suffix('"')?;
    let mut out = Vec::new();
    let mut chars = body.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\\' {
            let mut buffer = [0u8; 4];
            out.extend_from_slice(c.encode_utf8(&mut buffer).as_bytes());
            continue;
        }
        let escape = chars.next()?;
        let byte = match escape {
            'n' => b'\n',
            't' => b'\t',
            'r' => b'\r',
            'a' => 7,
            'b' => 8,
            'f' => 12,
            'v' => 11,
            'e' => 27,
            'x' => {
                let mut value = 0u32;
                while let Some(digit) = chars.peek().and_then(|c| c.to_digit(16)) {
                    value = value.wrapping_mul(16) + digit;
                    chars.next();
                }
                value as u8
            }
            '0'..='7' => {
                let mut value = escape.to_digit(8)?;
                for _ in 0..2 {
                    let Some(digit) = chars.peek().and_then(|c| c.to_digit(8)) else {
                        break;
                    };
                    value = value * 8 + digit;
                    chars.next();
                }
                value as u8
            }
            other => {
                let mut buffer = [0u8; 4];
                out.extend_from_slice(other.encode_utf8(&mut buffer).as_bytes());
                continue;
            }
        };
        out.push(byte);
    }
    Some(out)
}
