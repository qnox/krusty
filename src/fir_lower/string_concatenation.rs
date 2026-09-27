//! The flattened arguments of a string concatenation, as kotlinc's
//! `FlattenStringConcatenationLowering` collects them.
//!
//! A string template, a `String.plus` call (the member or the nullable-receiver extension) and a
//! `String` addition are one concatenation. Their operands that are themselves concatenations, or
//! an `Any?.toString()` call, contribute their own arguments in order, so `"a" + x + "$y!"` is the
//! single concatenation `["a", x, y, "!"]`.

use crate::fir::{
    FirBinaryOperation, FirBody, FirCallArgument, FirCallTarget, FirConversion, FirConversionKind,
    FirExprId, FirExprKind, FirIntrinsic,
};
use crate::types::Ty;

/// One argument of a flattened concatenation with the conversion the checker fixed for it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct ConcatenationPart {
    pub(super) value: FirExprId,
    pub(super) conversion: Option<FirConversion>,
}

/// The arguments of `expression` when it is a string concatenation, in evaluation order.
///
/// A left-nested `a + b + c + ...` chain is as deep as it is long, so the operands are expanded
/// from an explicit work stack rather than by recursion.
pub(super) fn flattened_concatenation(
    body: &FirBody,
    expression: FirExprId,
) -> Option<Vec<ConcatenationPart>> {
    let mut pending = concatenation_operands(body, expression)?;
    pending.reverse();
    let mut parts = Vec::new();
    while let Some(part) = pending.pop() {
        match concatenation_operands(body, part.value).or_else(|| to_string_operand(body, part)) {
            Some(operands) => pending.extend(operands.into_iter().rev()),
            None => parts.push(part),
        }
    }
    Some(parts)
}

/// The direct operands of a concatenation expression.
fn concatenation_operands(body: &FirBody, expression: FirExprId) -> Option<Vec<ConcatenationPart>> {
    let node = body.expr(expression)?;
    match &node.kind {
        FirExprKind::StringTemplate(parts) => Some(
            parts
                .iter()
                .map(|&value| ConcatenationPart {
                    value,
                    conversion: None,
                })
                .collect(),
        ),
        FirExprKind::Binary {
            operation: FirBinaryOperation::Add,
            lhs,
            rhs,
        } if node.ty.get().canonical_semantic() == Ty::String => Some(vec![
            ConcatenationPart {
                value: *lhs,
                conversion: None,
            },
            ConcatenationPart {
                value: *rhs,
                conversion: None,
            },
        ]),
        FirExprKind::Call(call)
            if matches!(
                call.target,
                FirCallTarget::Intrinsic {
                    operation: FirIntrinsic::StringPlus,
                    ..
                }
            ) =>
        {
            let receiver = call.dispatch_receiver.or(call.extension_receiver)?;
            let [FirCallArgument::Expression {
                value, conversion, ..
            }] = call.arguments.as_ref()
            else {
                return None;
            };
            Some(vec![
                ConcatenationPart {
                    value: receiver.value,
                    conversion: appended(receiver.conversion),
                },
                ConcatenationPart {
                    value: *value,
                    conversion: appended(*conversion),
                },
            ])
        }
        _ => None,
    }
}

/// The conversion an operand keeps once it is appended. `plus` takes `Any?`, but a concatenation
/// appends each operand at its own type, so the widening to that parameter is dropped; a smart cast
/// or platform-nullability check still applies to the operand.
fn appended(conversion: Option<FirConversion>) -> Option<FirConversion> {
    conversion.filter(|conversion| {
        !matches!(
            conversion.kind,
            FirConversionKind::NullabilityWidening { .. }
                | FirConversionKind::NumericWidening { .. }
        )
    })
}

/// The receiver of an `Any?.toString()` operand, which a concatenation appends itself.
fn to_string_operand(body: &FirBody, part: ConcatenationPart) -> Option<Vec<ConcatenationPart>> {
    let FirExprKind::Call(call) = &body.expr(part.value)?.kind else {
        return None;
    };
    if !matches!(
        call.target,
        FirCallTarget::Intrinsic {
            operation: FirIntrinsic::NullableAnyToString,
            ..
        }
    ) || !call.arguments.is_empty()
    {
        return None;
    }
    let receiver = call.dispatch_receiver.or(call.extension_receiver)?;
    Some(vec![ConcatenationPart {
        value: receiver.value,
        conversion: appended(receiver.conversion),
    }])
}
