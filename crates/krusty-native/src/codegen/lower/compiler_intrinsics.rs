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
        operation: krusty::backend::BackendCompilerIntrinsic,
        operand: u32,
        ret: Ty,
    ) -> Result<Option<Value>, Unsupported> {
        use krusty::backend::BackendCompilerIntrinsic;
        use krusty::libraries::PrimitiveUnaryIntrinsic;

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

    /// A builtin scalar's own `hashCode()`, by the rule Kotlin fixes for the receiver's type.
    fn primitive_hash_code(&mut self, operand: u32) -> Result<Option<Value>, Unsupported> {
        let Some(ty) = self.checked_type(operand) else {
            return Err("a primitive `hashCode` without a checked receiver type".into());
        };
        if !matches!(
            ty,
            Ty::Boolean
                | Ty::Char
                | Ty::Byte
                | Ty::Short
                | Ty::Int
                | Ty::Long
                | Ty::Float
                | Ty::Double
        ) {
            return Err(declined!("a primitive `hashCode` on `{ty:?}`"));
        }
        let Some(value) = self.coerce(operand, ty)? else {
            return Err("a primitive `hashCode` on `Unit`".into());
        };
        if self.terminated {
            return Ok(None);
        }
        self.value_hash(value, ty).map(Some)
    }

    /// Realize an exact provider intrinsic reached through an ordinary member call.
    ///
    /// Returns `None` only when the selected declaration has no operation handled here, allowing
    /// the caller to continue through ordinary dependency realization.
    pub(super) fn compiler_intrinsic_call(
        &mut self,
        site: u32,
        intrinsic: Option<krusty::backend::BackendCompilerIntrinsic>,
        receiver: Option<u32>,
        args: &[u32],
        declared: DeclaredOperands<'_>,
        ret: Ty,
    ) -> Option<Result<Option<Value>, Unsupported>> {
        let extension_receiver_parameter = declared.extension_receiver_parameter;
        if let Some(plan) = intrinsic.and_then(super::super::dependency_intrinsics::operation_plan)
        {
            use super::super::dependency_intrinsics::DependencyOperation;
            return Some(match plan {
                DependencyOperation::ListStorage(plan) => {
                    if receiver.is_some()
                        || extension_receiver_parameter.is_some()
                        || !ret.is_reference()
                    {
                        return Some(Err(
                            "a list storage construction with malformed receiver or result".into(),
                        ));
                    }
                    self.list_storage_construction(plan, args)
                }
                DependencyOperation::StringPlus => {
                    let Some((left, right)) =
                        exact_extension_pair(receiver, args, extension_receiver_parameter)
                    else {
                        return Some(Err("a malformed `String.plus`".into()));
                    };
                    if ret != Ty::obj("kotlin/String") {
                        return Some(Err("a `String.plus` with a non-String result".into()));
                    }
                    self.string_plus(left, right, ret)
                }
                DependencyOperation::PrimitiveHashCode => {
                    let (Some(operand), [], None) = (receiver, args, extension_receiver_parameter)
                    else {
                        return Some(Err("a malformed primitive `hashCode`".into()));
                    };
                    if ret != Ty::Int {
                        return Some(Err("a primitive `hashCode` with a non-Int result".into()));
                    }
                    self.primitive_hash_code(operand)
                }
            });
        }
        if intrinsic == Some(krusty::backend::BackendCompilerIntrinsic::ReinterpretBits) {
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
        if let Some(op) = intrinsic.and_then(IrBinOp::of_primitive_operator) {
            let (Some(lhs), [rhs]) = (receiver, args) else {
                return Some(Err("a primitive operator with malformed operands".into()));
            };
            return Some(self.binary(site, op, lhs, *rhs));
        }
        // `a.inv()`, `a.unaryMinus()` and `a.unaryPlus()` — the operators with no operand but
        // the receiver, at the result's carrier as the source operator forms are.
        if let Some(
            operation @ (krusty::backend::BackendCompilerIntrinsic::PrimitiveBitNot
            | krusty::backend::BackendCompilerIntrinsic::PrimitiveUnary(_)),
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
            Some(krusty::backend::BackendCompilerIntrinsic::CollectionSizeOrDefault) => {
                let Some((iterable, fallback)) =
                    exact_extension_pair(receiver, args, extension_receiver_parameter)
                else {
                    return Some(Err(
                        "an exact collection sizing helper with malformed operands".into(),
                    ));
                };
                if ret != Ty::Int || self.checked_type(fallback) != Some(Ty::Int) {
                    return Some(Err(
                        "an exact collection sizing helper with a mismatched signature".into(),
                    ));
                }
                return Some(self.collection_size_or_default(iterable, fallback));
            }
            Some(krusty::backend::BackendCompilerIntrinsic::TextTrim) => {
                let Some(value) =
                    exact_extension_operand(receiver, args, extension_receiver_parameter)
                else {
                    return Some(Err("an exact text trim with malformed operands".into()));
                };
                return Some(self.text_trim(value, ret));
            }
            Some(krusty::backend::BackendCompilerIntrinsic::ClosedRangeContains) => {
                let operands = match (receiver, args) {
                    (Some(range), [value]) => (range, *value),
                    (None, [range, value]) if extension_receiver_parameter == Some(0) => {
                        (*range, *value)
                    }
                    _ => return Some(Err("a range membership with malformed operands".into())),
                };
                return Some(self.facade_range_contains(operands.0, operands.1));
            }
            Some(
                operation @ (krusty::backend::BackendCompilerIntrinsic::RangeUntil
                | krusty::backend::BackendCompilerIntrinsic::RangeDownTo),
            ) => {
                let Some((start, end)) =
                    exact_extension_pair(receiver, args, extension_receiver_parameter)
                else {
                    return Some(Err("an exact range builder with malformed operands".into()));
                };
                return Some(self.intrinsic_range_builder(operation, start, end, ret));
            }
            Some(
                operation @ (krusty::backend::BackendCompilerIntrinsic::UnsignedDivide { operand }
                | krusty::backend::BackendCompilerIntrinsic::UnsignedRemainder {
                    operand,
                }),
            ) => {
                let (None, [left, right]) = (receiver, args) else {
                    return Some(Err("an unsigned division with malformed operands".into()));
                };
                if ret != operand {
                    return Some(Err("an unsigned division with a mismatched result".into()));
                }
                let remainder = matches!(
                    operation,
                    krusty::backend::BackendCompilerIntrinsic::UnsignedRemainder { .. }
                );
                return Some(self.unsigned_division(operand, remainder, *left, *right));
            }
            Some(krusty::backend::BackendCompilerIntrinsic::ResultFailure(operation)) => {
                if receiver.is_some() {
                    return Some(Err("a Result helper with a dispatch receiver".into()));
                }
                return Some(self.result_operation(operation, args, ret));
            }
            Some(krusty::backend::BackendCompilerIntrinsic::RadixToString { operand }) => {
                let (None, [value, radix]) = (receiver, args) else {
                    return Some(Err("a radix rendering with malformed operands".into()));
                };
                let symbol = match operand {
                    Ty::Int => "kt_int_to_string_radix",
                    Ty::Long => "kt_long_to_string_radix",
                    _ => return Some(Err("a radix rendering of an unsupported width".into())),
                };
                if ret != Ty::obj("kotlin/String") {
                    return Some(Err("a radix rendering with a non-String result".into()));
                }
                let params = [operand, Ty::Int];
                return Some((|| {
                    let arguments = self.arguments(&[*value, *radix], &params)?;
                    if self.terminated {
                        return Ok(None);
                    }
                    self.runtime_call(symbol, &params, ret, &arguments)
                })());
            }
            Some(krusty::backend::BackendCompilerIntrinsic::FloatingIsNaN { operand }) => {
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
            Some(krusty::backend::BackendCompilerIntrinsic::FloatingIsInfinite { operand }) => {
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
            Some(krusty::backend::BackendCompilerIntrinsic::ArrayGet) => {
                Some(IrIntrinsic::ArrayGet)
            }
            Some(krusty::backend::BackendCompilerIntrinsic::ArraySet) => {
                Some(IrIntrinsic::ArraySet)
            }
            Some(krusty::backend::BackendCompilerIntrinsic::ArraySize) => {
                Some(IrIntrinsic::ArraySize)
            }
            Some(krusty::backend::BackendCompilerIntrinsic::StringGet) => {
                Some(IrIntrinsic::StringGet)
            }
            Some(krusty::backend::BackendCompilerIntrinsic::StringLength) => {
                Some(IrIntrinsic::StringLength)
            }
            Some(krusty::backend::BackendCompilerIntrinsic::NullableAnyToString) => {
                Some(IrIntrinsic::NullableAnyToString)
            }
            _ => None,
        };
        if let Some(operation) = operation {
            return Some(self.intrinsic(operation, ret, receiver, args));
        }
        match intrinsic {
            Some(krusty::backend::BackendCompilerIntrinsic::BooleanNot) => {
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
            Some(krusty::backend::BackendCompilerIntrinsic::UnsignedCompare { carrier }) => {
                let (None, [left, right]) = (receiver, args) else {
                    return Some(Err("a malformed unsigned comparison".into()));
                };
                Some(self.unsigned_compare_intrinsic(carrier, *left, *right, ret))
            }
            // `a.compareTo(b)` on a builtin scalar, compared at the carrier the declaration's own
            // receiver and parameter promote to, as the source operator form is.
            Some(krusty::backend::BackendCompilerIntrinsic::PrimitiveCompare) => {
                let (Some(receiver), [argument], Some(owner), [parameter]) =
                    (receiver, args, declared.receiver, declared.params)
                else {
                    return Some(Err("a malformed primitive `compareTo`".into()));
                };
                let Some(operand) =
                    krusty::libraries::builtin_member_realization::primitive_compare_operand(
                        owner, *parameter,
                    )
                else {
                    return Some(Err(declined!(
                        "a primitive `compareTo` of `{owner:?}` with `{parameter:?}`"
                    )));
                };
                Some(self.primitive_compare(operand, receiver, *argument, ret))
            }
            _ => None,
        }
    }

    /// Both arguments are eager, including the fallback when Collection.size will be used.
    fn collection_size_or_default(
        &mut self,
        iterable: u32,
        fallback: u32,
    ) -> Result<Option<Value>, Unsupported> {
        let value = self.reference(iterable)?;
        if self.terminated {
            return Ok(None);
        }
        let Some(fallback) = self.coerce(fallback, Ty::Int)? else {
            return Err("a Unit fallback in collection sizing".into());
        };
        if self.terminated {
            return Ok(None);
        }
        self.runtime_call(
            "kt_collection_size_or_default",
            &[any(), Ty::Int],
            Ty::Int,
            &[value, fallback],
        )
    }
}

/// The selected declaration's own operands: the classifier owning a member, the declared value
/// parameters, and the argument coordinate a serialized body keeps an extension receiver at.
#[derive(Clone, Copy)]
pub(super) struct DeclaredOperands<'a> {
    pub(super) receiver: Option<Ty>,
    pub(super) params: &'a [Ty],
    pub(super) extension_receiver_parameter: Option<u32>,
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

/// An exact extension has one receiver and one value argument. The serialized form's
/// receiver remains at its authenticated parameter coordinate rather than becoming dispatch.
fn exact_extension_pair(
    receiver: Option<u32>,
    args: &[u32],
    extension_receiver_parameter: Option<u32>,
) -> Option<(u32, u32)> {
    match (receiver, args, extension_receiver_parameter) {
        (Some(receiver), [argument], None) => Some((receiver, *argument)),
        (None, [receiver, argument], Some(0)) => Some((*receiver, *argument)),
        _ => None,
    }
}

pub(super) fn console_intrinsic(
    intrinsic: Option<krusty::backend::BackendCompilerIntrinsic>,
) -> Option<super::super::super::intrinsics::ConsoleIntrinsic> {
    use super::super::super::intrinsics::ConsoleIntrinsic;
    use krusty::backend::BackendCompilerIntrinsic;

    match intrinsic {
        Some(BackendCompilerIntrinsic::Print) => Some(ConsoleIntrinsic::Print),
        Some(BackendCompilerIntrinsic::Println) => Some(ConsoleIntrinsic::Println),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::{exact_extension_operand, exact_extension_pair};

    #[test]
    fn an_extension_pair_preserves_its_two_authenticated_operand_layouts() {
        assert_eq!(exact_extension_pair(Some(3), &[5], None), Some((3, 5)));
        assert_eq!(exact_extension_pair(None, &[7, 11], Some(0)), Some((7, 11)));
        for (receiver, args, coordinate) in [
            (None, &[7, 11][..], None),
            (None, &[7, 11][..], Some(1)),
            (None, &[7][..], Some(0)),
            (None, &[7, 11, 13][..], Some(0)),
            (Some(3), &[][..], None),
            (Some(3), &[5, 7][..], None),
            (Some(3), &[5][..], Some(0)),
        ] {
            assert_eq!(
                exact_extension_pair(receiver, args, coordinate),
                None,
                "receiver={receiver:?}, args={args:?}, coordinate={coordinate:?}"
            );
        }
    }

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
