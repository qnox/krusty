//! JVM realization of exact builtin scalar members that have no JVM method.
//!
//! The provider tags a normalized builtin declaration (`Int.times`, `Long.shl`, `Boolean.not`) with
//! the compiler operation it denotes. Checked FIR publishes that operation for an ordinary call, but a
//! callable reference keeps the selected declaration identity: its adapter body calls the declaration
//! and its reflective carrier must still name it. This pass realizes such an exact declaration as the
//! common primitive operation, from the checked semantic receiver, parameter and result types. It
//! never selects a declaration and never looks at a spelling.

use crate::ir::{Callee, ExprId, IrBinOp, IrConst, IrExpr, IrFile, IrIntrinsic, IrTypeOp};
use crate::libraries::builtin_member_realization::{
    primitive_binary_operands, primitive_compare_operand,
};
use crate::libraries::{CompilerIntrinsic, PrimitiveBinaryIntrinsic, PrimitiveUnaryIntrinsic};
use crate::types::Ty;

/// Checked operands of one invocation of a builtin member.
pub(super) struct BuiltinMemberOperands<'a> {
    pub(super) receiver: ExprId,
    /// Semantic type of the selected dispatch receiver (`Int` for `Int.times`).
    pub(super) receiver_ty: Ty,
    pub(super) arguments: &'a [ExprId],
    /// Semantic declaration parameter types, one per argument.
    pub(super) parameters: &'a [Ty],
    pub(super) result: Ty,
}

/// The common IR operation implementing `intrinsic` for these operands, or `None` when the
/// intrinsic is not a scalar operation or the operands do not have its declared shape.
pub(super) fn operation(
    ir: &mut IrFile,
    intrinsic: CompilerIntrinsic,
    operands: BuiltinMemberOperands<'_>,
) -> Option<IrExpr> {
    let BuiltinMemberOperands {
        receiver,
        receiver_ty,
        arguments,
        parameters,
        result,
    } = operands;
    let receiver_ty = receiver_ty.canonical_semantic().non_null();
    let result = result.canonical_semantic().non_null();
    if arguments.len() != parameters.len() {
        return None;
    }
    match (arguments, parameters) {
        ([], []) => unary_operation(ir, intrinsic, receiver, receiver_ty, result),
        ([argument], [parameter]) => {
            let parameter = parameter.canonical_semantic().non_null();
            if intrinsic == CompilerIntrinsic::PrimitiveCompare {
                let operand = primitive_compare_operand(receiver_ty, parameter)?;
                if result != Ty::Int {
                    return None;
                }
                let lhs = coerce(ir, receiver, receiver_ty, operand);
                let rhs = coerce(ir, *argument, parameter, operand);
                return Some(IrExpr::Call {
                    callee: Callee::Intrinsic {
                        operation: IrIntrinsic::PrimitiveCompare {
                            operand,
                            relational_operator: false,
                        },
                        ret: Ty::Int,
                    },
                    dispatch_receiver: Some(lhs),
                    args: vec![rhs],
                });
            }
            let op = binary_operation(intrinsic)?;
            let (lhs_ty, rhs_ty) = primitive_binary_operands(intrinsic, parameter, result)?;
            // JVM arithmetic has no `char` form: `Char.plus(Int)` adds as `int` and narrows the sum.
            let arithmetic = |ty: Ty| if ty == Ty::Char { Ty::Int } else { ty };
            let lhs = coerce(ir, receiver, receiver_ty, arithmetic(lhs_ty));
            let rhs = coerce(ir, *argument, parameter, arithmetic(rhs_ty));
            let value = IrExpr::PrimitiveBinOp { op, lhs, rhs };
            if arithmetic(result) == result {
                return Some(value);
            }
            let value = ir.add_expr(value);
            Some(IrExpr::TypeOp {
                op: IrTypeOp::ImplicitCoercion,
                arg: value,
                type_operand: result,
            })
        }
        _ => None,
    }
}

fn unary_operation(
    ir: &mut IrFile,
    intrinsic: CompilerIntrinsic,
    receiver: ExprId,
    receiver_ty: Ty,
    result: Ty,
) -> Option<IrExpr> {
    match intrinsic {
        CompilerIntrinsic::BooleanNot if receiver_ty == Ty::Boolean && result == Ty::Boolean => {
            let operand = coerce(ir, receiver, receiver_ty, Ty::Boolean);
            let false_value = ir.add_expr(IrExpr::Const(IrConst::Boolean(false)));
            Some(IrExpr::PrimitiveBinOp {
                op: IrBinOp::Eq,
                lhs: operand,
                rhs: false_value,
            })
        }
        CompilerIntrinsic::NumericConversion => {
            let operand = coerce(ir, receiver, receiver_ty, receiver_ty);
            Some(IrExpr::TypeOp {
                op: IrTypeOp::ImplicitCoercion,
                arg: operand,
                type_operand: result,
            })
        }
        CompilerIntrinsic::PrimitiveUnary(PrimitiveUnaryIntrinsic::Identity) => {
            let operand = coerce(ir, receiver, receiver_ty, receiver_ty);
            Some(IrExpr::TypeOp {
                op: IrTypeOp::ImplicitCoercion,
                arg: operand,
                type_operand: result,
            })
        }
        CompilerIntrinsic::PrimitiveUnary(PrimitiveUnaryIntrinsic::Negate) => {
            let operand = coerce(ir, receiver, receiver_ty, result);
            Some(IrExpr::PrimitiveNeg {
                operand,
                ty: result,
            })
        }
        CompilerIntrinsic::PrimitiveBitNot if matches!(result, Ty::Int | Ty::Long) => {
            let operand = coerce(ir, receiver, receiver_ty, result);
            let all_bits = ir.add_expr(IrExpr::Const(if result == Ty::Long {
                IrConst::Long(-1)
            } else {
                IrConst::Int(-1)
            }));
            Some(IrExpr::PrimitiveBinOp {
                op: IrBinOp::BitXor,
                lhs: operand,
                rhs: all_bits,
            })
        }
        _ => None,
    }
}

fn binary_operation(intrinsic: CompilerIntrinsic) -> Option<IrBinOp> {
    Some(match intrinsic {
        CompilerIntrinsic::PrimitiveBinary(operation) => match operation {
            PrimitiveBinaryIntrinsic::Add => IrBinOp::Add,
            PrimitiveBinaryIntrinsic::Subtract => IrBinOp::Sub,
            PrimitiveBinaryIntrinsic::Multiply => IrBinOp::Mul,
            PrimitiveBinaryIntrinsic::Divide => IrBinOp::Div,
            PrimitiveBinaryIntrinsic::Remainder => IrBinOp::Rem,
        },
        CompilerIntrinsic::PrimitiveBitAnd => IrBinOp::BitAnd,
        CompilerIntrinsic::PrimitiveBitOr => IrBinOp::BitOr,
        CompilerIntrinsic::PrimitiveBitXor => IrBinOp::BitXor,
        CompilerIntrinsic::PrimitiveShiftLeft => IrBinOp::Shl,
        CompilerIntrinsic::PrimitiveShiftRight => IrBinOp::Shr,
        CompilerIntrinsic::PrimitiveUnsignedShiftRight => IrBinOp::Ushr,
        _ => return None,
    })
}

/// Bring one operand to its semantic scalar type, then to the operation's carrier. A reference
/// adapter may receive the value boxed (an erased generic `(acc: S, T) -> S` parameter or a platform
/// `Int!`), and a box must be unwrapped as its own type before it is widened (`Integer` to `long`).
fn coerce(ir: &mut IrFile, value: ExprId, semantic: Ty, carrier: Ty) -> ExprId {
    let unboxed = ir.add_expr(IrExpr::TypeOp {
        op: IrTypeOp::ImplicitCoercion,
        arg: value,
        type_operand: semantic,
    });
    if semantic == carrier {
        return unboxed;
    }
    ir.add_expr(IrExpr::TypeOp {
        op: IrTypeOp::ImplicitCoercion,
        arg: unboxed,
        type_operand: carrier,
    })
}
