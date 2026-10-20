//! Ordinary dependency-call forms of compiler-supplied operations.
//!
//! Source operators normally arrive as `IrIntrinsic`. A callable reference to the same selected
//! declaration, and every call of a target-stdlib intrinsic such as a bit reinterpretation, arrives
//! as an external call instead, retaining the provider's exact compiler-intrinsic fact. This
//! boundary consumes that identity; it never reconstructs one from an owner or callable spelling.

use super::*;

impl BodyLowering<'_, '_, '_> {
    /// The provider's bit reinterpretation of `operand`, a `from` scalar, as the equal-width `to`.
    fn reinterpret_bits(
        &mut self,
        operand: u32,
        from: Ty,
        to: Ty,
    ) -> Result<Option<Value>, Unsupported> {
        if !matches!(
            (from, to),
            (Ty::Float, Ty::Int)
                | (Ty::Double, Ty::Long)
                | (Ty::Int, Ty::Float)
                | (Ty::Long, Ty::Double)
        ) {
            return Err(declined!(
                "a scalar reinterpretation from `{from:?}` to `{to:?}`"
            ));
        }
        let Some(value) = self.coerce(operand, from)? else {
            return Err("a scalar reinterpretation of `Unit`".into());
        };
        if self.terminated {
            return Ok(None);
        }
        let clif = self
            .carrier(to)
            .clif()
            .expect("a validated reinterpretation target has a scalar carrier");
        Ok(Some(self.bits_of(value, clif)))
    }

    fn primitive_unary(
        &mut self,
        operation: crate::backend::BackendCompilerIntrinsic,
        operand: u32,
        ret: Ty,
    ) -> Result<Option<Value>, Unsupported> {
        use crate::backend::BackendCompilerIntrinsic;
        use crate::libraries::PrimitiveUnaryIntrinsic;

        let Some(value) = self.coerce(operand, ret)? else {
            return Err("a primitive unary operator on `Unit`".into());
        };
        if self.terminated {
            return Ok(None);
        }
        let floating = matches!(ret.non_null(), Ty::Float | Ty::Double);
        Ok(Some(match operation {
            BackendCompilerIntrinsic::PrimitiveBitNot if !floating => {
                self.builder.ins().bnot(value)
            }
            BackendCompilerIntrinsic::PrimitiveUnary(PrimitiveUnaryIntrinsic::Identity) => value,
            BackendCompilerIntrinsic::PrimitiveUnary(PrimitiveUnaryIntrinsic::Negate) => {
                if floating {
                    self.builder.ins().fneg(value)
                } else {
                    self.builder.ins().ineg(value)
                }
            }
            _ => {
                return Err(declined!(
                    "the primitive operator `{operation:?}` on `{ret:?}`"
                ))
            }
        }))
    }

    /// Realize an exact provider intrinsic reached through an ordinary member call.
    ///
    /// Returns `None` only when the selected declaration has no operation handled here, allowing
    /// the caller to continue through ordinary dependency realization.
    pub(super) fn compiler_intrinsic_call(
        &mut self,
        site: u32,
        intrinsic: Option<crate::backend::BackendCompilerIntrinsic>,
        receiver: Option<u32>,
        args: &[u32],
        extension_receiver_parameter: Option<u32>,
        ret: Ty,
    ) -> Option<Result<Option<Value>, Unsupported>> {
        if intrinsic == Some(crate::backend::BackendCompilerIntrinsic::ReinterpretBits) {
            let operand = match (receiver, args) {
                (Some(receiver), []) => receiver,
                (None, [argument]) => *argument,
                _ => {
                    return Some(Err(
                        "an exact scalar reinterpretation with malformed operands".into(),
                    ))
                }
            };
            let from = match self.checked_type(operand) {
                Some(from) => from.non_null(),
                None => {
                    return Some(Err(
                        "an exact scalar reinterpretation without a checked operand type".into(),
                    ))
                }
            };
            return Some(self.reinterpret_bits(operand, from, ret));
        }
        // A primitive operator member — `a.and(b)` in an expanded library body — is the binary
        // operation its source operator form lowers to.
        if let Some(op) = intrinsic.and_then(primitive_binary_operation) {
            let (Some(lhs), [rhs]) = (receiver, args) else {
                return Some(Err("a primitive operator with malformed operands".into()));
            };
            return Some(self.binary(site, op, lhs, *rhs));
        }
        // `a.inv()`, `a.unaryMinus()` and `a.unaryPlus()` — the operators with no operand but
        // the receiver, at the result's carrier as the source operator forms are.
        if let Some(
            operation @ (crate::backend::BackendCompilerIntrinsic::PrimitiveBitNot
            | crate::backend::BackendCompilerIntrinsic::PrimitiveUnary(_)),
        ) = intrinsic
        {
            let (Some(operand), []) = (receiver, args) else {
                return Some(Err(
                    "a primitive unary operator with malformed operands".into()
                ));
            };
            return Some(self.primitive_unary(operation, operand, ret));
        }
        let floating_extension_operand = |operand: Ty| {
            let value = exact_extension_operand(receiver, args, extension_receiver_parameter)?;
            (self.checked_type(value).map(Ty::non_null) == Some(operand)).then_some(value)
        };
        match intrinsic {
            Some(crate::backend::BackendCompilerIntrinsic::FloatingIsNaN { operand }) => {
                let Some(value) = floating_extension_operand(operand) else {
                    return Some(Err(
                        "an exact floating-point `isNaN` with malformed operands".into(),
                    ));
                };
                if ret != Ty::Boolean {
                    return Some(Err(
                        "an exact floating-point `isNaN` with a non-Boolean result".into(),
                    ));
                }
                return Some(self.float_predicate(
                    super::super::super::intrinsics::FloatPredicate::IsNaN,
                    value,
                ));
            }
            Some(crate::backend::BackendCompilerIntrinsic::FloatingIsInfinite { operand }) => {
                let Some(value) = floating_extension_operand(operand) else {
                    return Some(Err(
                        "an exact floating-point `isInfinite` with malformed operands".into(),
                    ));
                };
                if ret != Ty::Boolean {
                    return Some(Err(
                        "an exact floating-point `isInfinite` with a non-Boolean result".into(),
                    ));
                }
                return Some(self.float_predicate(
                    super::super::super::intrinsics::FloatPredicate::IsInfinite,
                    value,
                ));
            }
            _ => {}
        }
        let operation = match intrinsic {
            Some(crate::backend::BackendCompilerIntrinsic::ArrayGet) => Some(IrIntrinsic::ArrayGet),
            Some(crate::backend::BackendCompilerIntrinsic::ArraySet) => Some(IrIntrinsic::ArraySet),
            Some(crate::backend::BackendCompilerIntrinsic::ArraySize) => {
                Some(IrIntrinsic::ArraySize)
            }
            Some(crate::backend::BackendCompilerIntrinsic::StringGet) => {
                Some(IrIntrinsic::StringGet)
            }
            Some(crate::backend::BackendCompilerIntrinsic::StringLength) => {
                Some(IrIntrinsic::StringLength)
            }
            Some(crate::backend::BackendCompilerIntrinsic::NullableAnyToString) => {
                Some(IrIntrinsic::NullableAnyToString)
            }
            _ => None,
        };
        if let Some(operation) = operation {
            return Some(self.intrinsic(operation, ret, receiver, args));
        }
        match intrinsic {
            Some(crate::backend::BackendCompilerIntrinsic::StringPlus) => {
                let (Some(receiver), [argument]) = (receiver, args) else {
                    return Some(Err("a malformed `String.plus`".into()));
                };
                Some(self.string_plus(receiver, *argument, ret))
            }
            Some(crate::backend::BackendCompilerIntrinsic::BooleanNot) => {
                let (Some(receiver), []) = (receiver, args) else {
                    return Some(Err("a malformed `Boolean.not`".into()));
                };
                if self.checked_type(receiver).map(Ty::non_null) != Some(Ty::Boolean) {
                    return Some(Err(
                        "a `Boolean.not` receiver with a non-Boolean type".into()
                    ));
                }
                Some(self.boolean_not(receiver, ret))
            }
            Some(crate::backend::BackendCompilerIntrinsic::UnsignedCompare { carrier }) => {
                let (None, [left, right]) = (receiver, args) else {
                    return Some(Err("a malformed unsigned comparison".into()));
                };
                Some(self.unsigned_compare_intrinsic(carrier, *left, *right, ret))
            }
            _ => None,
        }
    }
}

/// The common-IR operation of a provider's primitive binary operator.
fn primitive_binary_operation(
    intrinsic: crate::backend::BackendCompilerIntrinsic,
) -> Option<IrBinOp> {
    use crate::backend::BackendCompilerIntrinsic;
    use crate::libraries::PrimitiveBinaryIntrinsic;

    Some(match intrinsic {
        BackendCompilerIntrinsic::PrimitiveBinary(operation) => match operation {
            PrimitiveBinaryIntrinsic::Add => IrBinOp::Add,
            PrimitiveBinaryIntrinsic::Subtract => IrBinOp::Sub,
            PrimitiveBinaryIntrinsic::Multiply => IrBinOp::Mul,
            PrimitiveBinaryIntrinsic::Divide => IrBinOp::Div,
            PrimitiveBinaryIntrinsic::Remainder => IrBinOp::Rem,
        },
        BackendCompilerIntrinsic::PrimitiveBitAnd => IrBinOp::BitAnd,
        BackendCompilerIntrinsic::PrimitiveBitOr => IrBinOp::BitOr,
        BackendCompilerIntrinsic::PrimitiveBitXor => IrBinOp::BitXor,
        BackendCompilerIntrinsic::PrimitiveShiftLeft => IrBinOp::Shl,
        BackendCompilerIntrinsic::PrimitiveShiftRight => IrBinOp::Shr,
        BackendCompilerIntrinsic::PrimitiveUnsignedShiftRight => IrBinOp::Ushr,
        _ => return None,
    })
}

/// The two common-IR representations of one exact top-level extension receiver. A checked source
/// edge carries it separately from value arguments. A serialized KLIB support body retains it at
/// its authenticated parameter coordinate. No other coordinate is an extension operand.
fn exact_extension_operand(
    receiver: Option<u32>,
    args: &[u32],
    extension_receiver_parameter: Option<u32>,
) -> Option<u32> {
    match (receiver, args, extension_receiver_parameter) {
        (Some(value), [], None) => Some(value),
        (None, [value], Some(0)) => Some(*value),
        _ => None,
    }
}

pub(super) fn console_intrinsic(
    intrinsic: Option<crate::backend::BackendCompilerIntrinsic>,
) -> Option<super::super::super::intrinsics::ConsoleIntrinsic> {
    use super::super::super::intrinsics::ConsoleIntrinsic;
    use crate::backend::BackendCompilerIntrinsic;

    match intrinsic {
        Some(BackendCompilerIntrinsic::Print) => Some(ConsoleIntrinsic::Print),
        Some(BackendCompilerIntrinsic::Println) => Some(ConsoleIntrinsic::Println),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::exact_extension_operand;

    #[test]
    fn a_floating_extension_accepts_only_its_two_authenticated_receiver_coordinates() {
        assert_eq!(exact_extension_operand(Some(3), &[], None), Some(3));
        assert_eq!(exact_extension_operand(None, &[5], Some(0)), Some(5));

        assert_eq!(exact_extension_operand(None, &[5], None), None);
        assert_eq!(exact_extension_operand(None, &[5], Some(1)), None);
        assert_eq!(exact_extension_operand(Some(3), &[5], None), None);
        assert_eq!(exact_extension_operand(Some(3), &[], Some(0)), None);
    }
}
