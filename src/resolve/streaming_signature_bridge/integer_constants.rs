//! Integer-constant semantics at the compact-signature resolver boundary.

use crate::fir::{ResolvedTy, SigBinaryOperator};
use crate::integer_constant::{IntegerConstant, IntegerConstantOp};
use crate::types::Ty;

pub(super) fn fold_selected_binary(
    operator: SigBinaryOperator,
    lhs_ty: ResolvedTy,
    lhs: IntegerConstant,
    rhs_ty: ResolvedTy,
    rhs: IntegerConstant,
    result: ResolvedTy,
) -> Option<IntegerConstant> {
    // Unsigned arithmetic is not a signature constant. An explicit `ULong` result of
    // `if (flag) value else 1u + 1u` is `Comparable<*>` on the reference compiler; inferring
    // that shape as `ULong` lowers the sum as `Long` and throws.
    let natural = match (lhs, rhs) {
        (
            IntegerConstant::Signed(_) | IntegerConstant::DivisionByZero,
            IntegerConstant::Signed(_) | IntegerConstant::DivisionByZero,
        ) => Ty::Int,
        (IntegerConstant::Unsigned(_), IntegerConstant::Unsigned(_)) => return None,
        _ => return None,
    };
    if lhs_ty.get() != natural || rhs_ty.get() != natural || result.get() != natural {
        return None;
    }
    let operation = match operator {
        SigBinaryOperator::Add => IntegerConstantOp::Add,
        SigBinaryOperator::Subtract => IntegerConstantOp::Subtract,
        SigBinaryOperator::Multiply => IntegerConstantOp::Multiply,
        SigBinaryOperator::Divide => IntegerConstantOp::Divide,
        SigBinaryOperator::Remainder => IntegerConstantOp::Remainder,
        SigBinaryOperator::Equal
        | SigBinaryOperator::NotEqual
        | SigBinaryOperator::Less
        | SigBinaryOperator::LessOrEqual
        | SigBinaryOperator::Greater
        | SigBinaryOperator::GreaterOrEqual
        | SigBinaryOperator::BooleanAnd
        | SigBinaryOperator::BooleanOr
        | SigBinaryOperator::ReferentialEqual
        | SigBinaryOperator::ReferentialNotEqual => return None,
    };
    lhs.fold(operation, rhs)
}

pub(super) fn adapted_branch(
    current: ResolvedTy,
    constant: IntegerConstant,
    sibling: ResolvedTy,
) -> Option<ResolvedTy> {
    let natural = current.get().non_null();
    let target = sibling.get().non_null();
    if !crate::symbol_resolver::CallArgKind::integer_constant(natural, constant)
        .adapts_integer_literal_to(target)
    {
        return None;
    }
    ResolvedTy::new(target).ok()
}
