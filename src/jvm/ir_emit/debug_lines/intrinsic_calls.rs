//! Which source calls kotlinc's JVM codegen realizes so that they state their own line before
//! their operands, decided here from the builtin-call, safe-call and elvis provenance common IR
//! keeps.
//!
//! kotlinc's intrinsic functions mark the call's line before they generate any operand. When the
//! receiver or first operand starts on another line (`h(\n a,\n b,\n).size`), that operand marks
//! its line at the same offset, and the line table keeps both entries. An ordinary call
//! (`String.length`, a property getter, `toString()`) marks its line only at its invoke, after its
//! operands. So does an inline one: a conversion that realizes an inlined getter (`Char.code`),
//! and every unsigned operator and conversion, which the standard library declares `inline` over
//! the signed carrier.

use crate::fir::{FirBinaryOperation, FirUnaryOperation, SyntheticOriginKind};
use crate::ir::{
    Callee, ExprId, IrBuiltinCall, IrBuiltinCallKind, IrExpr, IrFile, IrIntrinsic, IrNodeOrigin,
    IrSafeCallRead,
};
use crate::types::Ty;

/// Whether `expression` states its own line before its operands, keeping that entry where an
/// operand marks another line at the same offset.
pub(in crate::jvm::ir_emit) fn states_line_before_operands(
    ir: &IrFile,
    expression: ExprId,
) -> bool {
    if let Some(call) = ir.builtin_calls.get(&expression) {
        return is_intrinsic_call(ir, expression, call);
    }
    // A safe call states its line where it declares the receiver temporary, before the receiver,
    // unless an elvis folds it (`JvmSafeCallChainFoldingLowering`).
    ir.safe_calls.contains(&expression) && !is_elvis_operand(ir, expression)
}

/// Whether kotlinc realizes `call` as an intrinsic call of its own.
fn is_intrinsic_call(ir: &IrFile, expression: ExprId, call: &IrBuiltinCall) -> bool {
    let signed = !is_unsigned(call.result) && !call.operands.iter().any(|&ty| is_unsigned(ty));
    match call.kind {
        IrBuiltinCallKind::Unary(operation) => operation != FirUnaryOperation::Identity && signed,
        IrBuiltinCallKind::Binary(operation) => {
            !matches!(
                operation,
                FirBinaryOperation::BooleanAnd | FirBinaryOperation::BooleanOr
            ) && signed
        }
        // A widening or narrowing between two primitives. An unboxing is no intrinsic call, nor is
        // a conversion that realizes an inlined getter.
        IrBuiltinCallKind::NumericConversion => {
            let inlined = matches!(
                ir.fir_origins.get(&expression),
                Some(IrNodeOrigin::Synthetic {
                    kind: SyntheticOriginKind::InlinedCall,
                    ..
                })
            );
            let primitive_change = matches!(call.operands[..], [from]
                if from.is_numeric_or_char() && call.result.is_numeric_or_char()
                    && from != call.result);
            !inlined && primitive_change && signed
        }
        IrBuiltinCallKind::IndexedRead => true,
        IrBuiltinCallKind::Intrinsic => matches!(
            ir.expr(expression),
            IrExpr::Call { callee, .. } if matches!(
                **callee,
                Callee::Intrinsic {
                    operation: IrIntrinsic::ArraySize
                        | IrIntrinsic::ArrayGet
                        | IrIntrinsic::StringGet
                        | IrIntrinsic::PrimitiveCompare { .. },
                    ..
                }
            )
        ),
    }
}

fn is_unsigned(ty: Ty) -> bool {
    ty.non_null().canonical_semantic().is_unsigned()
}

/// Whether `value` is the left value an elvis's temporary holds.
fn is_elvis_operand(ir: &IrFile, value: ExprId) -> bool {
    ir.elvis_temporaries.keys().any(|&temporary| {
        matches!(ir.expr(temporary), IrExpr::Variable { init: Some(init), .. } if *init == value)
    })
}

/// The line `inv()`'s intrinsic restates where it pushes the all-bits mask, after its receiver.
pub(in crate::jvm::ir_emit) fn inversion_mask_line(ir: &IrFile, expression: ExprId) -> Option<u32> {
    let call = ir.builtin_calls.get(&expression)?;
    (call.kind == IrBuiltinCallKind::Unary(FirUnaryOperation::BitwiseNot)
        && is_intrinsic_call(ir, expression, call))
    .then(|| ir.expr_source_lines.get(&expression).copied())
    .flatten()
    .filter(|&line| line != 0)
}

/// The line a read of a safe call's receiver temporary carries: the null test at the safe call's
/// line, the selector's receiver at the line the receiver starts on.
pub(in crate::jvm::ir_emit) fn safe_call_read_line(ir: &IrFile, read: ExprId) -> Option<u32> {
    match ir.safe_call_reads.get(&read)? {
        IrSafeCallRead::NullCheck { line } => Some(*line),
        IrSafeCallRead::Receiver { start_line } => Some(*start_line),
    }
}
