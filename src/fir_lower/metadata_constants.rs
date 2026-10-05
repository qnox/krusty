//! Kotlin metadata's `HAS_CONSTANT` property fact, decided from the checked initializer.
//!
//! kotlinc's serializer sets it for a `const val`, or for a `val` whose declared type can be a
//! `const val` type and whose initializer passes `FirToConstantValueChecker`: a literal, a string
//! template or literal concatenation over constants, a read of a `const val`, and a number
//! conversion (`toByte` … `toDouble`, `toChar`) or `unaryMinus` applied to a constant. Any other
//! operation over constants (`1 + 2`, `!true`, `'a'.code`) is folded at run time but is not one.

use crate::fir::{
    FirBody, FirCallArgument, FirCallTarget, FirConstant, FirConversionKind, FirExprId,
    FirExprKind, FirIntrinsic, FirStatementKind, FirUnaryOperation,
};
use crate::types::Ty;

/// Whether a `val` of `declared` type initialized by `body` records `HAS_CONSTANT`.
pub(super) fn has_constant_initializer(body: &FirBody, declared: Ty) -> bool {
    can_be_used_for_const_val(declared)
        && initializer(body).is_some_and(|initializer| has_constant_value(body, initializer))
}

/// kotlinc's `canBeUsedForConstVal`: a non-null built-in scalar or `String`.
fn can_be_used_for_const_val(ty: Ty) -> bool {
    matches!(
        ty,
        Ty::Boolean
            | Ty::Char
            | Ty::Byte
            | Ty::Short
            | Ty::Int
            | Ty::Long
            | Ty::Float
            | Ty::Double
            | Ty::String
    ) || ty.is_unsigned()
}

/// The single expression a property initializer body evaluates.
fn initializer(body: &FirBody) -> Option<FirExprId> {
    let [root] = body.roots() else {
        return None;
    };
    match body.statement(*root)?.kind {
        FirStatementKind::Expression(expression) => Some(expression),
        _ => None,
    }
}

fn has_constant_value(body: &FirBody, expression: FirExprId) -> bool {
    let Some(node) = body.expr(expression) else {
        return false;
    };
    match &node.kind {
        FirExprKind::Constant(constant) => !matches!(constant, FirConstant::Null),
        // `-x` is `x.unaryMinus()`, a number operation over any constant.
        FirExprKind::Unary {
            operation: FirUnaryOperation::Negate,
            operand,
        } => has_constant_value(body, *operand),
        // `+3` is a literal to kotlinc's parser; `+X` stays a `unaryPlus` call.
        FirExprKind::Unary {
            operation: FirUnaryOperation::Identity,
            operand,
        } => is_literal(body, *operand),
        FirExprKind::ImplicitConversion { value, conversion } => match conversion.kind {
            // An explicit `toX()` call; unsigned conversions are extensions, not number conversions.
            FirConversionKind::NumericConversion { to } => {
                !to.get().canonical_semantic().is_unsigned() && has_constant_value(body, *value)
            }
            FirConversionKind::NumericWidening { .. }
            | FirConversionKind::NullabilityWidening { .. } => has_constant_value(body, *value),
            _ => false,
        },
        FirExprKind::StringTemplate(parts) => {
            parts.iter().all(|part| has_constant_value(body, *part))
        }
        FirExprKind::Call(_) => string_plus_operands(body, expression).is_some_and(|(lhs, rhs)| {
            is_string_concatenation_operand(body, lhs)
                && is_string_concatenation_operand(body, rhs)
                && has_constant_value(body, lhs)
                && has_constant_value(body, rhs)
        }),
        _ => false,
    }
}

/// A literal as source wrote it, never the value of a constant declaration it reads.
fn is_literal(body: &FirBody, expression: FirExprId) -> bool {
    matches!(
        body.expr(expression).map(|node| &node.kind),
        Some(FirExprKind::Constant(constant)) if !matches!(constant, FirConstant::Null)
    ) && !body.is_constant_read(expression)
}

/// kotlinc's parser merges `+` between string literals and templates into one concatenation;
/// a `+` with any other operand, a `const val` read included, stays a `plus` call.
fn is_string_concatenation_operand(body: &FirBody, expression: FirExprId) -> bool {
    match body.expr(expression).map(|node| &node.kind) {
        Some(FirExprKind::Constant(FirConstant::String(_))) => is_literal(body, expression),
        Some(FirExprKind::StringTemplate(_)) => true,
        Some(FirExprKind::Call(_)) => {
            string_plus_operands(body, expression).is_some_and(|(lhs, rhs)| {
                is_string_concatenation_operand(body, lhs)
                    && is_string_concatenation_operand(body, rhs)
            })
        }
        _ => false,
    }
}

/// The receiver and argument of a checked `String.plus`, the operation `+` on a string selects.
fn string_plus_operands(body: &FirBody, expression: FirExprId) -> Option<(FirExprId, FirExprId)> {
    let FirExprKind::Call(call) = &body.expr(expression)?.kind else {
        return None;
    };
    let FirCallTarget::Intrinsic {
        operation: FirIntrinsic::StringPlus,
        ..
    } = &call.target
    else {
        return None;
    };
    let receiver = call
        .dispatch_receiver
        .filter(|receiver| receiver.conversion.is_none())?;
    // `plus(other: Any?)` widens its argument; the value itself is unchanged.
    let [FirCallArgument::Expression {
        value, conversion, ..
    }] = call.arguments.as_ref()
    else {
        return None;
    };
    if conversion.is_some_and(|conversion| {
        !matches!(
            conversion.kind,
            FirConversionKind::NullabilityWidening { .. }
        )
    }) {
        return None;
    }
    Some((receiver.value, *value))
}
