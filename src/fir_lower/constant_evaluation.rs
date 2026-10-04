//! Compile-time evaluation of checked operations whose operands are constants.
//!
//! This is kotlinc's `ConstEvaluationLowering` in `EvaluationMode.OnlyIntrinsicConst` over the
//! operations checked FIR publishes. kotlinc folds a call to a declaration marked
//! `@IntrinsicConstEvaluation`, and the language operators `==`, `<`, `<=`, `>`, `>=`, `&&` and `||`,
//! once every argument is a constant, and it merges the constant arguments of a string template.
//! The checker publishes exactly those builtin declarations as typed operations (`Binary`,
//! `Unary`, a numeric conversion, a `String` intrinsic), so evaluation reads only those operations
//! and the checked types; it never selects a declaration or inspects a source spelling.
//!
//! An operation kotlinc's interpreter would reject is left alone: an integer division by zero or a
//! referential comparison.

use std::collections::HashMap;

use crate::kt_string::{KtString, KtStringBuf};
use crate::types::Ty;

use super::string_concatenation::ConcatenationPart;
use crate::fir::{
    FirBinaryOperation, FirBody, FirCallArgument, FirCallTarget, FirConstant, FirConversion,
    FirConversionKind, FirExprId, FirExprKind, FirIntrinsic, FirUnaryOperation,
};

/// A constant value together with the type it has as a value, before any widening to a nullable
/// or supertype slot the checker placed it in.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct EvaluatedConstant {
    pub(super) value: FirConstant,
    pub(super) ty: Ty,
}

/// One argument of a folded string concatenation.
#[derive(Clone, Debug, PartialEq)]
pub(super) enum TemplateRun {
    Constant(FirConstant),
    Part(ConcatenationPart),
}

/// Memoized constant values of one checked body's expressions.
///
/// Lowering asks for the value of each operation it reaches, outermost first, so a subtree whose
/// root is not constant is asked again for each of its operations; the memo keeps that linear.
#[derive(Default)]
pub(super) struct ConstantEvaluation {
    values: HashMap<FirExprId, Option<EvaluatedConstant>>,
    depth: u32,
}

/// How many nested evaluations run between checks that the stack can take more.
const STACK_CHECK_INTERVAL: u32 = 64;

impl ConstantEvaluation {
    /// The constant `expression` evaluates to, when it is an operation kotlinc folds and every
    /// operand is itself constant. A literal is not an operation and yields `None`.
    pub(super) fn fold(
        &mut self,
        body: &FirBody,
        expression: FirExprId,
    ) -> Option<EvaluatedConstant> {
        let kind = &body.expr(expression)?.kind;
        let operation = match kind {
            FirExprKind::Unary { .. }
            | FirExprKind::Binary { .. }
            | FirExprKind::Equality { .. }
            | FirExprKind::StringTemplate(_)
            | FirExprKind::Call(_) => true,
            // An explicit conversion call (`300.toByte()`) is intrinsic-const; an implicit widening
            // (`d + 1.0F`) is not a call kotlinc evaluates and keeps its runtime widening.
            FirExprKind::ImplicitConversion { conversion, .. } => {
                matches!(conversion.kind, FirConversionKind::NumericConversion { .. })
            }
            _ => false,
        };
        if !operation {
            return None;
        }
        let constant = self.evaluate(body, expression)?;
        // The folded value replaces the whole expression, so it must already have the
        // expression's own type; a constant seen through a widening keeps its narrower type and
        // is folded at the operation inside instead.
        (constant.ty == body.expr(expression)?.ty.get().canonical_semantic()).then_some(constant)
    }

    /// A flattened concatenation's parts with each run of constant parts merged into one
    /// `String` constant, as kotlinc folds a string concatenation whose neighbouring arguments
    /// are constants or operations over constants.
    pub(super) fn template_runs(
        &mut self,
        body: &FirBody,
        parts: &[ConcatenationPart],
    ) -> Vec<TemplateRun> {
        let mut runs = Vec::with_capacity(parts.len());
        let mut run: Option<KtStringBuf> = None;
        for &part in parts {
            let text = self.evaluate(body, part.value).and_then(|constant| {
                let mut text = KtStringBuf::new();
                push_text(&constant, &mut text)?;
                Some(text.finish())
            });
            match text {
                Some(text) => run.get_or_insert_with(KtStringBuf::new).push_kt(&text),
                None => {
                    if let Some(text) = run.take() {
                        runs.push(TemplateRun::Constant(FirConstant::String(text.finish())));
                    }
                    runs.push(TemplateRun::Part(part));
                }
            }
        }
        if let Some(text) = run {
            runs.push(TemplateRun::Constant(FirConstant::String(text.finish())));
        }
        runs
    }

    fn evaluate(&mut self, body: &FirBody, expression: FirExprId) -> Option<EvaluatedConstant> {
        if let Some(known) = self.values.get(&expression) {
            return known.clone();
        }
        // Evaluation recurses over an operand chain as deep as the checked nesting, ahead of the
        // lowering dispatcher's own stack checks, so it grows the stack on the same schedule.
        self.depth = self
            .depth
            .checked_add(1)
            .expect("constant nesting exceeds u32");
        let value = if self.depth == 1 || self.depth.is_multiple_of(STACK_CHECK_INTERVAL) {
            crate::wide_stack::on_wide_stack(|| self.compute(body, expression))
        } else {
            self.compute(body, expression)
        };
        self.depth -= 1;
        self.values.insert(expression, value.clone());
        value
    }

    fn compute(&mut self, body: &FirBody, expression: FirExprId) -> Option<EvaluatedConstant> {
        let node = body.expr(expression)?;
        let ty = node.ty.get().canonical_semantic();
        match &node.kind {
            FirExprKind::Constant(value) => Some(EvaluatedConstant {
                value: value.clone(),
                ty: if matches!(value, FirConstant::Null) {
                    ty
                } else {
                    ty.non_null()
                },
            }),
            FirExprKind::ImplicitConversion { value, conversion } => {
                let value = self.evaluate(body, *value)?;
                convert(value, *conversion)
            }
            FirExprKind::Unary { operation, operand } => {
                let operand = self.evaluate(body, *operand)?;
                unary(*operation, operand, ty)
            }
            FirExprKind::Equality {
                operation,
                lhs,
                rhs,
                ..
            }
            | FirExprKind::Binary {
                operation,
                lhs,
                rhs,
            } => {
                let lhs = self.evaluate(body, *lhs)?;
                let rhs = self.evaluate(body, *rhs)?;
                binary(*operation, lhs, rhs, ty)
            }
            FirExprKind::StringTemplate(parts) => {
                let mut text = KtStringBuf::new();
                for part in parts.iter() {
                    push_text(&self.evaluate(body, *part)?, &mut text)?;
                }
                Some(string(text.finish()))
            }
            FirExprKind::Call(call) => {
                if let Some(name) = enum_entry_name(body, call) {
                    return Some(name);
                }
                let FirCallTarget::Intrinsic { operation, .. } = &call.target else {
                    return None;
                };
                let receiver = call
                    .dispatch_receiver
                    .or(call.extension_receiver)
                    .map(|receiver| self.converted(body, receiver.value, receiver.conversion));
                let arguments = call
                    .arguments
                    .iter()
                    .map(|argument| match argument {
                        FirCallArgument::Expression {
                            value, conversion, ..
                        } => self.converted(body, *value, *conversion),
                        FirCallArgument::Default { .. } | FirCallArgument::Vararg { .. } => None,
                    })
                    .collect::<Option<Vec<_>>>()?;
                intrinsic(operation, receiver?, &arguments)
            }
            _ => None,
        }
    }

    fn converted(
        &mut self,
        body: &FirBody,
        value: FirExprId,
        conversion: Option<FirConversion>,
    ) -> Option<EvaluatedConstant> {
        let value = self.evaluate(body, value)?;
        match conversion {
            Some(conversion) => convert(value, conversion),
            None => Some(value),
        }
    }
}

/// `Enum.name` on a direct entry is that entry's declaration name. The entry expression is not
/// itself a constant: evaluating it would initialize the enum. A name read through any other
/// receiver stays a call.
fn enum_entry_name(body: &FirBody, call: &crate::fir::FirCall) -> Option<EvaluatedConstant> {
    let FirCallTarget::Intrinsic {
        operation: FirIntrinsic::EnumName,
        ..
    } = &call.target
    else {
        return None;
    };
    let receiver = call.dispatch_receiver?;
    if receiver.conversion.is_some() {
        return None;
    }
    let FirExprKind::EnumEntry { name, .. } = &body.expr(receiver.value)?.kind else {
        return None;
    };
    Some(string(KtString::from(name.as_ref())))
}

fn string(value: KtString) -> EvaluatedConstant {
    EvaluatedConstant {
        value: FirConstant::String(value),
        ty: Ty::String,
    }
}

fn boolean(value: bool) -> EvaluatedConstant {
    EvaluatedConstant {
        value: FirConstant::Boolean(value),
        ty: Ty::Boolean,
    }
}

fn convert(value: EvaluatedConstant, conversion: FirConversion) -> Option<EvaluatedConstant> {
    match conversion.kind {
        FirConversionKind::NumericConversion { to } | FirConversionKind::NumericWidening { to } => {
            numeric(&value, to.get().canonical_semantic().non_null())
        }
        // A value placed in a nullable or supertype slot keeps its value; the operation that
        // consumes it reads it at its own type.
        FirConversionKind::NullabilityWidening { .. } | FirConversionKind::SmartCast { .. } => {
            Some(value)
        }
        _ => None,
    }
}

/// A numeric constant as the JVM holds it: integral types as a sign-extended `i64`, `Char` as its
/// code.
enum Number {
    Integral(i64),
    Float(f32),
    Double(f64),
}

fn number(value: &EvaluatedConstant) -> Option<Number> {
    Some(match (&value.value, value.ty) {
        (FirConstant::Int(value), Ty::Byte | Ty::Short | Ty::Int) => Number::Integral(*value),
        (FirConstant::Long(value), Ty::Long) => Number::Integral(*value),
        (FirConstant::Char(value), Ty::Char) => Number::Integral(i64::from(*value)),
        (FirConstant::Float(value), Ty::Float) => Number::Float(*value),
        (FirConstant::Double(value), Ty::Double) => Number::Double(*value),
        _ => return None,
    })
}

/// Kotlin's `toByte()`/`toInt()`/`toDouble()`/... on a constant: integral narrowing truncates,
/// a floating value converts to an integral type rounding toward zero and saturating, NaN to 0.
fn numeric(value: &EvaluatedConstant, to: Ty) -> Option<EvaluatedConstant> {
    let integral = |value: i64| -> Option<FirConstant> {
        Some(match to {
            Ty::Byte => FirConstant::Int(i64::from(value as i8)),
            Ty::Short => FirConstant::Int(i64::from(value as i16)),
            Ty::Int => FirConstant::Int(i64::from(value as i32)),
            Ty::Long => FirConstant::Long(value),
            Ty::Char => FirConstant::Char(value as u16),
            Ty::Float => FirConstant::Float(value as f32),
            Ty::Double => FirConstant::Double(value as f64),
            _ => return None,
        })
    };
    let converted = match number(value)? {
        Number::Integral(value) => integral(value)?,
        Number::Float(value) => match to {
            Ty::Float => FirConstant::Float(value),
            Ty::Double => FirConstant::Double(f64::from(value)),
            // `Float.toLong()` saturates at `Long`'s range; the narrower integral conversions go
            // through `toInt()` first, as `Float.toByte()` is defined.
            Ty::Long => FirConstant::Long(value as i64),
            _ => integral(i64::from(value as i32))?,
        },
        Number::Double(value) => match to {
            Ty::Float => FirConstant::Float(value as f32),
            Ty::Double => FirConstant::Double(value),
            Ty::Long => FirConstant::Long(value as i64),
            _ => integral(i64::from(value as i32))?,
        },
    };
    Some(EvaluatedConstant {
        value: converted,
        ty: to,
    })
}

fn unary(
    operation: FirUnaryOperation,
    operand: EvaluatedConstant,
    ty: Ty,
) -> Option<EvaluatedConstant> {
    let ty = ty.non_null();
    if operation == FirUnaryOperation::BooleanNot {
        let FirConstant::Boolean(value) = operand.value else {
            return None;
        };
        return Some(boolean(!value));
    }
    // A signed literal is negated before it is narrowed to the checked result type. This ordering
    // is observable at the lower bound: `val b: Byte = -128` has an `Int` literal `128` under the
    // unary operation, and narrowing that magnitude first produces `-128`, whose negation is the
    // out-of-range `128`. Kotlin instead treats the signed source literal as one constant and only
    // then records its `Byte` identity.
    if operation == FirUnaryOperation::Negate {
        let negated = match operand.value {
            FirConstant::Int(value) => FirConstant::Int(value.wrapping_neg()),
            FirConstant::Long(value) => FirConstant::Long(value.wrapping_neg()),
            FirConstant::Float(value) => FirConstant::Float(-value),
            FirConstant::Double(value) => FirConstant::Double(-value),
            _ => return None,
        };
        return numeric(
            &EvaluatedConstant {
                value: negated,
                ty: operand.ty,
            },
            ty,
        );
    }
    let operand = numeric(&operand, ty)?;
    let value = match (operation, operand.value) {
        (FirUnaryOperation::Identity, value) => value,
        (FirUnaryOperation::BitwiseNot, FirConstant::Int(value)) if ty == Ty::Int => {
            FirConstant::Int(!value)
        }
        (FirUnaryOperation::BitwiseNot, FirConstant::Long(value)) => FirConstant::Long(!value),
        (FirUnaryOperation::Increment | FirUnaryOperation::Decrement, value) => {
            let step = if operation == FirUnaryOperation::Increment {
                1
            } else {
                -1
            };
            return arithmetic(
                FirBinaryOperation::Add,
                EvaluatedConstant { value, ty },
                EvaluatedConstant {
                    value: FirConstant::Int(step),
                    ty: Ty::Int,
                },
                ty,
            );
        }
        _ => return None,
    };
    Some(EvaluatedConstant { value, ty })
}

fn binary(
    operation: FirBinaryOperation,
    lhs: EvaluatedConstant,
    rhs: EvaluatedConstant,
    ty: Ty,
) -> Option<EvaluatedConstant> {
    let ty = ty.non_null();
    match operation {
        FirBinaryOperation::Add if ty == Ty::String => {
            let mut text = KtStringBuf::new();
            push_text(&lhs, &mut text)?;
            push_text(&rhs, &mut text)?;
            Some(string(text.finish()))
        }
        FirBinaryOperation::Add
        | FirBinaryOperation::Subtract
        | FirBinaryOperation::Multiply
        | FirBinaryOperation::Divide
        | FirBinaryOperation::Remainder
        | FirBinaryOperation::BitwiseAnd
        | FirBinaryOperation::BitwiseOr
        | FirBinaryOperation::BitwiseXor
        | FirBinaryOperation::ShiftLeft
        | FirBinaryOperation::ShiftRight
        | FirBinaryOperation::UnsignedShiftRight => arithmetic(operation, lhs, rhs, ty),
        FirBinaryOperation::Less
        | FirBinaryOperation::LessOrEqual
        | FirBinaryOperation::Greater
        | FirBinaryOperation::GreaterOrEqual => {
            let order = compare(&lhs, &rhs)?;
            // An unordered pair (a NaN operand) makes every relational comparison false.
            Some(boolean(order.is_some_and(|order| match operation {
                FirBinaryOperation::Less => order.is_lt(),
                FirBinaryOperation::LessOrEqual => order.is_le(),
                FirBinaryOperation::Greater => order.is_gt(),
                _ => order.is_ge(),
            })))
        }
        FirBinaryOperation::Equal | FirBinaryOperation::NotEqual => {
            let equal = equals(&lhs, &rhs)?;
            Some(boolean(equal == (operation == FirBinaryOperation::Equal)))
        }
        FirBinaryOperation::BooleanAnd | FirBinaryOperation::BooleanOr => {
            let (FirConstant::Boolean(lhs), FirConstant::Boolean(rhs)) = (lhs.value, rhs.value)
            else {
                return None;
            };
            Some(boolean(if operation == FirBinaryOperation::BooleanAnd {
                lhs && rhs
            } else {
                lhs || rhs
            }))
        }
        FirBinaryOperation::ReferentialEqual | FirBinaryOperation::ReferentialNotEqual => None,
    }
}

/// An arithmetic, bitwise or shift operation producing `ty`. `Char` arithmetic adds or subtracts
/// an `Int` offset (`Char - Char` produces an `Int`); every other operation has both operands
/// already converted to its result type by the checker.
fn arithmetic(
    operation: FirBinaryOperation,
    lhs: EvaluatedConstant,
    rhs: EvaluatedConstant,
    ty: Ty,
) -> Option<EvaluatedConstant> {
    if let (FirConstant::Boolean(lhs), FirConstant::Boolean(rhs)) = (&lhs.value, &rhs.value) {
        return Some(boolean(match operation {
            FirBinaryOperation::BitwiseAnd => lhs & rhs,
            FirBinaryOperation::BitwiseOr => lhs | rhs,
            FirBinaryOperation::BitwiseXor => lhs ^ rhs,
            _ => return None,
        }));
    }
    let value = match (number(&lhs)?, number(&rhs)?) {
        (Number::Integral(lhs), Number::Integral(rhs)) => match ty {
            Ty::Int | Ty::Char => {
                let (lhs, rhs) = (lhs as i32, rhs as i32);
                let value = match operation {
                    FirBinaryOperation::Add => lhs.wrapping_add(rhs),
                    FirBinaryOperation::Subtract => lhs.wrapping_sub(rhs),
                    FirBinaryOperation::Multiply => lhs.wrapping_mul(rhs),
                    FirBinaryOperation::Divide => (rhs != 0).then(|| lhs.wrapping_div(rhs))?,
                    FirBinaryOperation::Remainder => (rhs != 0).then(|| lhs.wrapping_rem(rhs))?,
                    FirBinaryOperation::BitwiseAnd => lhs & rhs,
                    FirBinaryOperation::BitwiseOr => lhs | rhs,
                    FirBinaryOperation::BitwiseXor => lhs ^ rhs,
                    FirBinaryOperation::ShiftLeft => lhs.wrapping_shl(rhs as u32),
                    FirBinaryOperation::ShiftRight => lhs.wrapping_shr(rhs as u32),
                    FirBinaryOperation::UnsignedShiftRight => {
                        (lhs as u32).wrapping_shr(rhs as u32) as i32
                    }
                    _ => return None,
                };
                if ty == Ty::Char {
                    FirConstant::Char(value as u16)
                } else {
                    FirConstant::Int(i64::from(value))
                }
            }
            Ty::Long => FirConstant::Long(match operation {
                FirBinaryOperation::Add => lhs.wrapping_add(rhs),
                FirBinaryOperation::Subtract => lhs.wrapping_sub(rhs),
                FirBinaryOperation::Multiply => lhs.wrapping_mul(rhs),
                FirBinaryOperation::Divide => (rhs != 0).then(|| lhs.wrapping_div(rhs))?,
                FirBinaryOperation::Remainder => (rhs != 0).then(|| lhs.wrapping_rem(rhs))?,
                FirBinaryOperation::BitwiseAnd => lhs & rhs,
                FirBinaryOperation::BitwiseOr => lhs | rhs,
                FirBinaryOperation::BitwiseXor => lhs ^ rhs,
                // A `Long` shift takes an `Int` distance.
                FirBinaryOperation::ShiftLeft => lhs.wrapping_shl(rhs as u32),
                FirBinaryOperation::ShiftRight => lhs.wrapping_shr(rhs as u32),
                FirBinaryOperation::UnsignedShiftRight => {
                    (lhs as u64).wrapping_shr(rhs as u32) as i64
                }
                _ => return None,
            }),
            _ => return None,
        },
        (Number::Float(lhs), Number::Float(rhs)) if ty == Ty::Float => {
            FirConstant::Float(floating(operation, lhs, rhs)?)
        }
        (Number::Double(lhs), Number::Double(rhs)) if ty == Ty::Double => {
            FirConstant::Double(floating(operation, lhs, rhs)?)
        }
        _ => return None,
    };
    Some(EvaluatedConstant { value, ty })
}

fn floating<T>(operation: FirBinaryOperation, lhs: T, rhs: T) -> Option<T>
where
    T: std::ops::Add<Output = T>
        + std::ops::Sub<Output = T>
        + std::ops::Mul<Output = T>
        + std::ops::Div<Output = T>
        + std::ops::Rem<Output = T>,
{
    Some(match operation {
        FirBinaryOperation::Add => lhs + rhs,
        FirBinaryOperation::Subtract => lhs - rhs,
        FirBinaryOperation::Multiply => lhs * rhs,
        FirBinaryOperation::Divide => lhs / rhs,
        FirBinaryOperation::Remainder => lhs % rhs,
        _ => return None,
    })
}

/// The IEEE order of two numeric or `Char` constants of one type; `Some(None)` when a NaN makes
/// them unordered.
fn compare(lhs: &EvaluatedConstant, rhs: &EvaluatedConstant) -> Option<Option<std::cmp::Ordering>> {
    if lhs.ty != rhs.ty {
        return None;
    }
    Some(match (number(lhs)?, number(rhs)?) {
        (Number::Integral(lhs), Number::Integral(rhs)) => Some(lhs.cmp(&rhs)),
        (Number::Float(lhs), Number::Float(rhs)) => lhs.partial_cmp(&rhs),
        (Number::Double(lhs), Number::Double(rhs)) => lhs.partial_cmp(&rhs),
        _ => return None,
    })
}

/// Kotlin `==` between two constants of one type: IEEE equality for a floating type, value
/// equality otherwise, and `null == null`.
fn equals(lhs: &EvaluatedConstant, rhs: &EvaluatedConstant) -> Option<bool> {
    match (&lhs.value, &rhs.value) {
        (FirConstant::Null, FirConstant::Null) => return Some(true),
        (FirConstant::String(lhs), FirConstant::String(rhs)) => return Some(lhs == rhs),
        (FirConstant::Boolean(lhs), FirConstant::Boolean(rhs)) => return Some(lhs == rhs),
        _ => {}
    }
    compare(lhs, rhs).map(|order| order.is_some_and(std::cmp::Ordering::is_eq))
}

fn intrinsic(
    operation: &FirIntrinsic,
    receiver: Option<EvaluatedConstant>,
    arguments: &[EvaluatedConstant],
) -> Option<EvaluatedConstant> {
    match (operation, receiver?, arguments) {
        (FirIntrinsic::StringLength, receiver, []) => {
            let FirConstant::String(text) = receiver.value else {
                return None;
            };
            Some(EvaluatedConstant {
                value: FirConstant::Int(i64::try_from(text.len_utf16()).ok()?),
                ty: Ty::Int,
            })
        }
        // An index outside the string throws at run time, which kotlinc's interpreter leaves to
        // the call.
        (FirIntrinsic::StringGet, receiver, [index]) => {
            let (FirConstant::String(text), FirConstant::Int(index)) =
                (&receiver.value, &index.value)
            else {
                return None;
            };
            let unit = text.units().nth(usize::try_from(*index).ok()?)?;
            Some(EvaluatedConstant {
                value: FirConstant::Char(unit),
                ty: Ty::Char,
            })
        }
        (FirIntrinsic::StringPlus, receiver, [other]) => {
            let mut text = KtStringBuf::new();
            push_text(&receiver, &mut text)?;
            push_text(other, &mut text)?;
            Some(string(text.finish()))
        }
        (FirIntrinsic::PrimitiveCompare { .. }, receiver, [other]) => {
            // `compareTo` orders a floating pair totally (`-0.0 < 0.0`, NaN above everything),
            // unlike the relational operators, and `false` below `true`.
            let order = match (&receiver.value, &other.value) {
                (FirConstant::Boolean(lhs), FirConstant::Boolean(rhs)) => lhs.cmp(rhs),
                _ => match (number(&receiver)?, number(other)?) {
                    (Number::Integral(lhs), Number::Integral(rhs)) => lhs.cmp(&rhs),
                    (Number::Float(lhs), Number::Float(rhs)) => lhs.total_cmp_kotlin(rhs),
                    (Number::Double(lhs), Number::Double(rhs)) => lhs.total_cmp_kotlin(rhs),
                    _ => return None,
                },
            };
            Some(EvaluatedConstant {
                value: FirConstant::Int(order as i64),
                ty: Ty::Int,
            })
        }
        _ => None,
    }
}

/// `Float.compareTo`/`Double.compareTo`: Java's `Float.compare`, which orders `-0.0` below `0.0`
/// and every NaN (as one value) above positive infinity.
trait KotlinTotalOrder {
    fn total_cmp_kotlin(self, other: Self) -> std::cmp::Ordering;
}

impl KotlinTotalOrder for f32 {
    fn total_cmp_kotlin(self, other: Self) -> std::cmp::Ordering {
        f64::from(self).total_cmp_kotlin(f64::from(other))
    }
}

impl KotlinTotalOrder for f64 {
    fn total_cmp_kotlin(self, other: Self) -> std::cmp::Ordering {
        match (self.is_nan(), other.is_nan()) {
            (true, true) => std::cmp::Ordering::Equal,
            (true, false) => std::cmp::Ordering::Greater,
            (false, true) => std::cmp::Ordering::Less,
            (false, false) => self.total_cmp(&other),
        }
    }
}

/// Append a constant's `toString()` text.
fn push_text(constant: &EvaluatedConstant, text: &mut KtStringBuf) -> Option<()> {
    match &constant.value {
        FirConstant::String(value) => text.push_kt(value),
        FirConstant::Null => text.push_str("null"),
        FirConstant::Boolean(value) => text.push_str(if *value { "true" } else { "false" }),
        FirConstant::Char(value) => text.push_unit(*value),
        FirConstant::Int(value) | FirConstant::Long(value) => text.push_str(&value.to_string()),
        FirConstant::UInt(value) => text.push_str(
            &match constant.ty.non_null().canonical_semantic() {
                Ty::UByte => u64::from(*value as u8),
                Ty::UShort => u64::from(*value as u16),
                Ty::UInt => u64::from(*value as u32),
                _ => return None,
            }
            .to_string(),
        ),
        FirConstant::ULong(value) if constant.ty.non_null().canonical_semantic() == Ty::ULong => {
            text.push_str(&(*value as u64).to_string())
        }
        FirConstant::ULong(_) => return None,
        FirConstant::Float(value) => crate::kt_string::push_f32(*value, text)?,
        FirConstant::Double(value) => crate::kt_string::push_f64(*value, text)?,
    }
    Some(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rendered(value: FirConstant, ty: Ty) -> Option<String> {
        let mut text = KtStringBuf::new();
        push_text(&EvaluatedConstant { value, ty }, &mut text)?;
        text.finish().as_str().map(str::to_owned)
    }

    #[test]
    fn narrow_unsigned_constants_render_at_their_checked_width() {
        assert_eq!(
            rendered(FirConstant::UInt(i64::from(u32::MAX)), Ty::UByte),
            Some("255".to_string())
        );
        assert_eq!(
            rendered(FirConstant::UInt(i64::from(u32::MAX)), Ty::UShort),
            Some("65535".to_string())
        );
        assert_eq!(
            rendered(FirConstant::UInt(i64::from(u32::MAX)), Ty::UInt),
            Some("4294967295".to_string())
        );
    }

    #[test]
    fn signed_minimum_literals_are_negated_before_narrowing() {
        let minimum = |magnitude, ty| {
            unary(
                FirUnaryOperation::Negate,
                EvaluatedConstant {
                    value: FirConstant::Int(magnitude),
                    ty: Ty::Int,
                },
                ty,
            )
            .expect("a signed literal folds")
        };

        let byte = minimum(128, Ty::Byte);
        assert_eq!(byte.value, FirConstant::Int(-128));
        assert_eq!(byte.ty, Ty::Byte);

        let short = minimum(32_768, Ty::Short);
        assert_eq!(short.value, FirConstant::Int(-32_768));
        assert_eq!(short.ty, Ty::Short);
    }
}
