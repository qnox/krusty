//! Checked FIR construction for provider-selected compiler intrinsics.

use crate::fir::{FirBinaryOperation, FirIntrinsic};
use crate::libraries::CompilerIntrinsic;
use crate::types::{AssertionMode, Ty};

pub(super) fn binary_operation(intrinsic: CompilerIntrinsic) -> Option<FirBinaryOperation> {
    Some(match intrinsic {
        CompilerIntrinsic::PrimitiveBinary(operation) => match operation {
            crate::libraries::PrimitiveBinaryIntrinsic::Add => FirBinaryOperation::Add,
            crate::libraries::PrimitiveBinaryIntrinsic::Subtract => FirBinaryOperation::Subtract,
            crate::libraries::PrimitiveBinaryIntrinsic::Multiply => FirBinaryOperation::Multiply,
            crate::libraries::PrimitiveBinaryIntrinsic::Divide => FirBinaryOperation::Divide,
            crate::libraries::PrimitiveBinaryIntrinsic::Remainder => FirBinaryOperation::Remainder,
        },
        CompilerIntrinsic::PrimitiveBitAnd => FirBinaryOperation::BitwiseAnd,
        CompilerIntrinsic::PrimitiveBitOr => FirBinaryOperation::BitwiseOr,
        CompilerIntrinsic::PrimitiveBitXor => FirBinaryOperation::BitwiseXor,
        CompilerIntrinsic::PrimitiveShiftLeft => FirBinaryOperation::ShiftLeft,
        CompilerIntrinsic::PrimitiveShiftRight => FirBinaryOperation::ShiftRight,
        CompilerIntrinsic::PrimitiveUnsignedShiftRight => FirBinaryOperation::UnsignedShiftRight,
        CompilerIntrinsic::ArrayFactory(_)
        | CompilerIntrinsic::ReinterpretBits
        | CompilerIntrinsic::ScalarInstruction(_)
        | CompilerIntrinsic::FloatingIsNaN { .. }
        | CompilerIntrinsic::FloatingIsInfinite { .. }
        | CompilerIntrinsic::ArrayGet
        | CompilerIntrinsic::ArraySet
        | CompilerIntrinsic::ArraySize
        | CompilerIntrinsic::CharCode
        | CompilerIntrinsic::StringLength
        | CompilerIntrinsic::StringGet
        | CompilerIntrinsic::StringPlus
        | CompilerIntrinsic::NullableAnyToString
        | CompilerIntrinsic::NumericConversion
        | CompilerIntrinsic::PrimitiveUnary(_)
        | CompilerIntrinsic::PrimitiveCompare
        | CompilerIntrinsic::BooleanNot
        | CompilerIntrinsic::PrimitiveHashCode
        | CompilerIntrinsic::PrimitiveEquals
        | CompilerIntrinsic::PrimitiveBitNot
        | CompilerIntrinsic::Assert
        | CompilerIntrinsic::AssertFailsWith
        | CompilerIntrinsic::Print
        | CompilerIntrinsic::Println
        | CompilerIntrinsic::StartCoroutine
        | CompilerIntrinsic::CoroutineContext
        | CompilerIntrinsic::CoroutineSuspended
        | CompilerIntrinsic::SuspendCoroutineUninterceptedOrReturn
        | CompilerIntrinsic::EnumValues
        | CompilerIntrinsic::EnumValueOf
        | CompilerIntrinsic::EnumEntries
        | CompilerIntrinsic::EnumName
        | CompilerIntrinsic::TypeOf
        | CompilerIntrinsic::IsEmpty
        | CompilerIntrinsic::IsNotEmpty
        | CompilerIntrinsic::Count
        | CompilerIntrinsic::TrimIndent
        | CompilerIntrinsic::TrimMargin
        | CompilerIntrinsic::FloatingRangeMembership
        | CompilerIntrinsic::ClosedRangeContains
        | CompilerIntrinsic::ComparableRangeTo
        | CompilerIntrinsic::RangeDownTo
        | CompilerIntrinsic::RangeUntil
        | CompilerIntrinsic::ProgressionStep
        | CompilerIntrinsic::ProgressionReversed
        | CompilerIntrinsic::WithIndex
        | CompilerIntrinsic::UnsignedCompare { .. }
        | CompilerIntrinsic::PrimitiveIteratorNext => return None,
    })
}

/// Operation and operand types of an exact compiler-supplied primitive binary declaration. The
/// operand carriers are the provider-owned rule shared with target realization of references.
pub(super) fn binary_semantics(
    intrinsic: CompilerIntrinsic,
    parameter: Ty,
    result: Ty,
) -> Option<(FirBinaryOperation, Ty, Ty)> {
    let operation = binary_operation(intrinsic)?;
    let (receiver, argument) =
        crate::libraries::builtin_member_realization::primitive_binary_operands(
            intrinsic, parameter, result,
        )?;
    Some((operation, receiver, argument))
}

/// Construct the intrinsic operation carried by a selected top-level declaration.
///
/// Selection and authentication have already happened at the provider boundary. A target-stdlib
/// intrinsic is not one of these: its call stays an ordinary call the target emitter realizes.
pub(super) fn selected_top_level(
    operation: Option<CompilerIntrinsic>,
    assertion_mode: AssertionMode,
    callee: &str,
) -> Option<FirIntrinsic> {
    match operation {
        Some(CompilerIntrinsic::Assert) => Some(FirIntrinsic::Assert {
            mode: assertion_mode,
        }),
        Some(CompilerIntrinsic::SuspendCoroutineUninterceptedOrReturn) => {
            Some(FirIntrinsic::SuspendCoroutineUninterceptedOrReturn {
                callee: callee.into(),
            })
        }
        _ => None,
    }
}
