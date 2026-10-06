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

/// The common IR operation realizing one builtin member, with the role the replacement plays.
pub(super) enum BuiltinMemberOperation {
    /// An ordinary scalar value.
    Value(IrExpr),
    /// `Boolean.not()`: the negation of this Boolean operand, which the replacement must carry as
    /// a recorded negation (see `IrFile::add_negation`), not as an equality with `false`.
    Negation(ExprId),
}

impl BuiltinMemberOperation {
    /// Replace the call node `destination` with this operation, committing the node and any
    /// provenance its role carries together.
    pub(super) fn commit(self, ir: &mut IrFile, destination: ExprId) {
        match self {
            Self::Value(value) => ir.exprs[destination as usize] = value,
            Self::Negation(operand) => ir.replace_with_negation(destination, operand),
        }
    }
}

/// The common IR operation implementing `intrinsic` for these operands, or `None` when the
/// intrinsic is not a scalar operation or the operands do not have its declared shape.
pub(super) fn operation(
    ir: &mut IrFile,
    intrinsic: CompilerIntrinsic,
    operands: BuiltinMemberOperands<'_>,
) -> Option<BuiltinMemberOperation> {
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
                return Some(BuiltinMemberOperation::Value(IrExpr::Call {
                    callee: Callee::Intrinsic {
                        operation: IrIntrinsic::PrimitiveCompare {
                            operand,
                            relational_operator: false,
                        },
                        ret: Ty::Int,
                    },
                    dispatch_receiver: Some(lhs),
                    args: vec![rhs],
                }));
            }
            let op = binary_operation(intrinsic)?;
            let (lhs_ty, rhs_ty) = primitive_binary_operands(intrinsic, parameter, result)?;
            // JVM arithmetic has no `char` form: `Char.plus(Int)` adds as `int` and narrows the sum.
            let arithmetic = |ty: Ty| if ty == Ty::Char { Ty::Int } else { ty };
            let lhs = coerce(ir, receiver, receiver_ty, arithmetic(lhs_ty));
            let rhs = coerce(ir, *argument, parameter, arithmetic(rhs_ty));
            let value = IrExpr::PrimitiveBinOp { op, lhs, rhs };
            if arithmetic(result) == result {
                return Some(BuiltinMemberOperation::Value(value));
            }
            let value = ir.add_expr(value);
            Some(BuiltinMemberOperation::Value(IrExpr::TypeOp {
                op: IrTypeOp::ImplicitCoercion,
                arg: value,
                type_operand: result,
            }))
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
) -> Option<BuiltinMemberOperation> {
    let value = match intrinsic {
        CompilerIntrinsic::BooleanNot if receiver_ty == Ty::Boolean && result == Ty::Boolean => {
            let operand = coerce(ir, receiver, receiver_ty, Ty::Boolean);
            return Some(BuiltinMemberOperation::Negation(operand));
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
        CompilerIntrinsic::PrimitiveBitNot
            if matches!(result, Ty::Int | Ty::Long | Ty::UInt | Ty::ULong) =>
        {
            let carrier = if matches!(result, Ty::Long | Ty::ULong) {
                Ty::Long
            } else {
                Ty::Int
            };
            let operand = coerce(ir, receiver, receiver_ty, carrier);
            let all_bits = ir.add_expr(IrExpr::Const(if carrier == Ty::Long {
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
    };
    value.map(BuiltinMemberOperation::Value)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_realized_boolean_not_replaces_its_call_with_a_recorded_negation() {
        let mut ir = IrFile::default();
        let receiver = ir.add_expr(IrExpr::Const(IrConst::Boolean(true)));
        let call = ir.add_expr(IrExpr::Const(IrConst::Boolean(false)));
        operation(
            &mut ir,
            CompilerIntrinsic::BooleanNot,
            BuiltinMemberOperands {
                receiver,
                receiver_ty: Ty::Boolean,
                arguments: &[],
                parameters: &[],
                result: Ty::Boolean,
            },
        )
        .expect("`Boolean.not()` is a scalar operation")
        .commit(&mut ir, call);

        let operand = ir
            .negated_operand(call)
            .expect("the replaced call is a recorded negation");
        let IrExpr::TypeOp {
            op: IrTypeOp::ImplicitCoercion,
            arg,
            type_operand: Ty::Boolean,
        } = ir.expr(operand)
        else {
            panic!("the negation's operand is the receiver as a `Boolean`");
        };
        assert_eq!(*arg, receiver);
        assert_eq!(ir.negations.len(), 1);
    }

    #[test]
    fn a_realized_scalar_value_is_not_a_negation() {
        let mut ir = IrFile::default();
        let receiver = ir.add_expr(IrExpr::Const(IrConst::Int(1)));
        let call = ir.add_expr(IrExpr::Const(IrConst::Int(0)));
        operation(
            &mut ir,
            CompilerIntrinsic::PrimitiveBitNot,
            BuiltinMemberOperands {
                receiver,
                receiver_ty: Ty::Int,
                arguments: &[],
                parameters: &[],
                result: Ty::Int,
            },
        )
        .expect("`Int.inv()` is a scalar operation")
        .commit(&mut ir, call);

        assert!(matches!(
            ir.expr(call),
            IrExpr::PrimitiveBinOp {
                op: IrBinOp::BitXor,
                ..
            }
        ));
        assert!(ir.negations.is_empty());
    }

    #[test]
    fn an_unsigned_bitwise_result_remains_a_semantic_primitive_operation() {
        let mut ir = IrFile::default();
        let receiver = ir.add_expr(IrExpr::Const(IrConst::Int(1)));
        let argument = ir.add_expr(IrExpr::Const(IrConst::Int(7)));
        let call = ir.add_expr(IrExpr::Const(IrConst::Int(0)));
        operation(
            &mut ir,
            CompilerIntrinsic::PrimitiveUnsignedShiftRight,
            BuiltinMemberOperands {
                receiver,
                receiver_ty: Ty::UInt,
                arguments: &[argument],
                parameters: &[Ty::Int],
                result: Ty::UInt,
            },
        )
        .expect("`UInt.shr` is a scalar operation")
        .commit(&mut ir, call);

        assert!(matches!(
            ir.expr(call),
            IrExpr::PrimitiveBinOp {
                op: IrBinOp::Ushr,
                ..
            }
        ));

        let long_receiver = ir.add_expr(IrExpr::Const(IrConst::Long(1)));
        let inverted = ir.add_expr(IrExpr::Const(IrConst::Long(0)));
        operation(
            &mut ir,
            CompilerIntrinsic::PrimitiveBitNot,
            BuiltinMemberOperands {
                receiver: long_receiver,
                receiver_ty: Ty::ULong,
                arguments: &[],
                parameters: &[],
                result: Ty::ULong,
            },
        )
        .expect("`ULong.inv` is a scalar operation")
        .commit(&mut ir, inverted);
        assert!(matches!(
            ir.expr(inverted),
            IrExpr::PrimitiveBinOp {
                op: IrBinOp::BitXor,
                ..
            }
        ));
    }
}
