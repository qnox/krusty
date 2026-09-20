//! Kotlin's arithmetic, comparison and numeric-representation rules, lowered.
//!
//! What lives here is every decision about a NUMBER: how a value moves between machine carriers,
//! which operand type an operation is performed in, and what the answer's type is. Kotlin's rules
//! differ from C's at almost every one of those points, and the differences are observable:
//!
//! - `+`, `-`, `*` and unary `-` WRAP; `/` and `%` go through the runtime so division by zero is
//!   Kotlin's `ArithmeticException` rather than a machine trap; shifts mask their count.
//! - Arithmetic on the narrow integer types IS `Int` arithmetic — Kotlin has no
//!   `Byte.plus(Byte): Byte` — and `Char` is the exception that makes the rest a rule, since
//!   `Char.plus(Int)` is declared to return `Char` and only `Char.minus(Char)` returns `Int`.
//! - `compareTo` on floating point is a TOTAL order, not C's `<`/`>`: every `NaN` above every
//!   other value including itself, `-0.0` below `0.0`. The same operation reached through `<`
//!   keeps IEEE semantics, so the two spellings genuinely differ.
//! - A float-to-integer conversion SATURATES with `NaN` to zero, which is what `fcvt_to_sint_sat`
//!   gives and what the machine's plain conversion does not.

use super::*;

impl BodyLowering<'_, '_, '_> {
    /// Convert a scalar between carriers, as Kotlin's `toInt()`/`toFloat()`/`toChar()` family and
    /// its widening conversions do: integers extend by the source's signedness or truncate;
    /// integer to float rounds; float to integer SATURATES with `NaN` to zero (`fcvt_to_sint_sat`
    /// is exactly Kotlin's rule, where the machine's plain conversion would trap or produce the
    /// indefinite value); a narrower integer target goes through `Int` first, as `Double.toByte()`
    /// is defined to.
    pub(super) fn resize(&mut self, value: Value, from: Type, signed: bool, to: Type) -> Value {
        if from == to {
            return value;
        }
        match (from.is_float(), to.is_float()) {
            (false, false) => {
                if from.bits() < to.bits() {
                    if signed {
                        self.builder.ins().sextend(to, value)
                    } else {
                        self.builder.ins().uextend(to, value)
                    }
                } else {
                    self.builder.ins().ireduce(to, value)
                }
            }
            (false, true) => {
                if signed {
                    self.builder.ins().fcvt_from_sint(to, value)
                } else {
                    self.builder.ins().fcvt_from_uint(to, value)
                }
            }
            (true, false) => {
                let wide = if to.bits() > 32 {
                    types::I64
                } else {
                    types::I32
                };
                let integer = self.builder.ins().fcvt_to_sint_sat(wide, value);
                if wide == to {
                    integer
                } else {
                    self.builder.ins().ireduce(to, integer)
                }
            }
            (true, true) => {
                if to.bits() > from.bits() {
                    self.builder.ins().fpromote(to, value)
                } else {
                    self.builder.ins().fdemote(to, value)
                }
            }
        }
    }

    /// An operator's operand as the primitive it is: a reference-carried one is a box, opened to
    /// the type its own bound names.
    fn unboxed_operand(&mut self, value: Value, ty: Option<Ty>) -> Result<Value, Unsupported> {
        let Some(ty) = ty else {
            return Ok(value);
        };
        if carrier(ty) != Carrier::Ref {
            return Ok(value);
        }
        let Some(scalar) = scalar_bound(ty) else {
            return Err("an operator on a reference operand".to_string());
        };
        Ok(self
            .convert(value, Some(any()), scalar)?
            .expect("a scalar target yields a value"))
    }

    /// Two scalar operands brought to one width, as Kotlin's operator overloads do (`Byte + Int`
    /// is `Int + Int`). Returns the values, their common type, and whether comparisons are signed.
    fn unify(
        &mut self,
        lhs: Value,
        lhs_ty: Option<Ty>,
        rhs: Value,
        rhs_ty: Option<Ty>,
    ) -> Result<(Value, Value, Type, bool), Unsupported> {
        // Kotlin's arithmetic and comparison operators are the PRIMITIVE ones, so an operand whose
        // type carries as a reference here is a boxed primitive: a value of a generic type whose
        // bound is a number, which the JVM boxes for the same reason. It has to be unboxed before
        // either side's machine type means anything — this function unifies by machine type, and a
        // pointer and an `Int` unify into pointer arithmetic that reads like an answer and is not
        // one.
        let lhs = self.unboxed_operand(lhs, lhs_ty)?;
        let rhs = self.unboxed_operand(rhs, rhs_ty)?;
        let left = self.builder.func.dfg.value_type(lhs);
        let right = self.builder.func.dfg.value_type(rhs);
        if left.is_float() != right.is_float() {
            return Err("an operator mixing integer and floating-point operands".to_string());
        }
        // `Char` and the four unsigned integers widen by zero-extension; everything else by sign.
        let signed_of = |ty: Option<Ty>| {
            !matches!(ty, Some(Ty::Char)) && !ty.is_some_and(|ty| ty.non_null().is_unsigned())
        };
        let width = if left.bits() >= right.bits() {
            left
        } else {
            right
        };
        let lhs = self.resize(lhs, left, signed_of(lhs_ty), width);
        let rhs = self.resize(rhs, right, signed_of(rhs_ty), width);
        // `Char` compares unsigned only against another `Char`: widened to `Int` it is a
        // non-negative `Int` and a signed comparison is the same thing. An UNSIGNED integer
        // compares unsigned at its own width, where the difference is the whole point — the top bit
        // is a value there and a sign everywhere else.
        let unsigned = |ty: Option<Ty>| ty.is_some_and(|ty| ty.non_null().is_unsigned());
        let both_chars = matches!(lhs_ty, Some(Ty::Char)) && matches!(rhs_ty, Some(Ty::Char));
        let signed = !(both_chars || unsigned(lhs_ty) || unsigned(rhs_ty));
        Ok((lhs, rhs, width, signed))
    }

    /// A built-in binary operator, with Kotlin's semantics where the machine's differ.
    pub(super) fn binary(
        &mut self,
        op: IrBinOp,
        lhs: u32,
        rhs: u32,
    ) -> Result<Option<Value>, Unsupported> {
        let lhs_ty = self.type_of(lhs);
        let rhs_ty = self.type_of(rhs);

        if matches!(op, IrBinOp::Eq | IrBinOp::Ne) {
            let against_null = matches!(self.file.ir.expr(lhs), IrExpr::Const(IrConst::Null))
                || matches!(self.file.ir.expr(rhs), IrExpr::Const(IrConst::Null));
            let on_references = lhs_ty.map(carrier) == Some(Carrier::Ref)
                || rhs_ty.map(carrier) == Some(Carrier::Ref);
            if against_null {
                // `x == null` is `x === null` in Kotlin: no `equals` is ever called.
                let left = self.reference(lhs)?;
                let right = self.reference(rhs)?;
                let condition = comparison(op, true).expect("equality");
                return Ok(Some(self.builder.ins().icmp(condition, left, right)));
            }
            if self.is_function_expression(lhs) || self.is_function_expression(rhs) {
                // Kotlin compares two callable references by the DECLARATION they name and the
                // receiver they bind, so `::f == ::f` is true even though each `::f` is its own
                // object. The generator gives one `kotlin.Any`'s identity equality, which answers
                // that `false` — so the comparison is declined rather than answered wrongly. What
                // it needs is one emitted type per referenced declaration, with an `equals` that
                // compares the type and the bound receiver.
                return Err("equality on a function value".to_string());
            }
            // An IEEE comparison where an operand arrived BOXED. Kotlin's
            // `ProperIeee754Comparisons` compares two floating-point operands by IEEE rules — so
            // `NaN == NaN` is false — whenever both static types are the floating-point type
            // itself, INCLUDING through a type parameter bounded by it (`fun <T : Double> f(d:
            // Double, v: T) { d == v }`) and through its nullable form. `kt_equals` is the total
            // order instead, where `NaN` equals `NaN`, and that is the right answer only once an
            // operand widens to something like `Any`.
            //
            // The difference is invisible for two unboxed operands, which take the scalar path
            // below and already compare by IEEE. It shows exactly here, where one side is a
            // reference and the rule still applies. Answering with `kt_equals` is wrong rather
            // than imprecise, so the comparison is declined until it is unboxed and compared
            // properly; `ieee754/equalsNaN_properIeeeComparisons.kt` is the case.
            if on_references
                && [lhs_ty, rhs_ty]
                    .iter()
                    .all(|ty| ty.is_some_and(is_ieee_operand))
            {
                return Err(
                    "an IEEE floating-point comparison with an operand that arrived boxed"
                        .to_string(),
                );
            }
            if on_references {
                // Kotlin's `==` on references is `equals`, dispatched through the receiver's
                // vtable and null-safe in Kotlin's sense (`null` equals only `null`) — which is
                // exactly what `kt_equals` is. A scalar on either side boxes, because `any == 5`
                // means `any?.equals(5)` there too.
                let left = self.reference(lhs)?;
                let right = self.reference(rhs)?;
                if self.terminated {
                    return Ok(None);
                }
                let equal = self
                    .runtime_call("kt_equals", &[any(), any()], Ty::Boolean, &[left, right])?
                    .expect("`kt_equals` returns a Boolean");
                return Ok(Some(if op == IrBinOp::Ne {
                    let one = self.builder.ins().iconst(types::I8, 1);
                    self.builder.ins().bxor(equal, one)
                } else {
                    equal
                }));
            }
            if lhs_ty.is_none() && rhs_ty.is_none() {
                // Neither a known scalar nor a known reference: either equality would be a guess.
                return Err("an equality on an undetermined operand type".to_string());
            }
        }
        if matches!(op, IrBinOp::RefEq | IrBinOp::RefNe) {
            // `===` between two values of a primitive type compares the VALUES (Kotlin: identity
            // equality on primitives is `==`, with a deprecation warning); boxing each side and
            // comparing the boxes' addresses would say `0L !== 0L`. Floating-point identity has
            // its own rules (`-0.0`, `NaN`) that nothing here implements yet, so it is declined.
            let both_scalars = matches!(lhs_ty.map(carrier), Some(Carrier::Scalar(..)))
                && matches!(rhs_ty.map(carrier), Some(Carrier::Scalar(..)));
            if both_scalars {
                let Some(left) = self.expression(lhs)? else {
                    return Err("a `Unit` operand".to_string());
                };
                let Some(right) = self.expression(rhs)? else {
                    return Err("a `Unit` operand".to_string());
                };
                if self.terminated {
                    return Ok(None);
                }
                let (left, right, ty, _) = self.unify(left, lhs_ty, right, rhs_ty)?;
                if ty.is_float() {
                    return Err("identity equality on floating-point values".to_string());
                }
                let condition = comparison(op, true).expect("identity");
                return Ok(Some(self.builder.ins().icmp(condition, left, right)));
            }
            let left = self.reference(lhs)?;
            let right = self.reference(rhs)?;
            let condition = comparison(op, true).expect("identity");
            return Ok(Some(self.builder.ins().icmp(condition, left, right)));
        }

        let Some(left) = self.expression(lhs)? else {
            return Err("a `Unit` operand".to_string());
        };
        let Some(right) = self.expression(rhs)? else {
            return Err("a `Unit` operand".to_string());
        };
        if self.terminated {
            return Ok(None);
        }

        // Shifts take an `Int` count whatever the left operand's width; Cranelift masks the count
        // to the operand width, which is exactly Kotlin's rule (`1 shl 32 == 1`).
        if matches!(op, IrBinOp::Shl | IrBinOp::Shr | IrBinOp::Ushr) {
            if self.builder.func.dfg.value_type(left).is_float() {
                return Err("a shift of a floating-point operand".to_string());
            }
            let left = self.widen_narrow_integer(left, lhs_ty);
            return Ok(Some(match op {
                IrBinOp::Shl => self.builder.ins().ishl(left, right),
                IrBinOp::Shr => self.builder.ins().sshr(left, right),
                _ => self.builder.ins().ushr(left, right),
            }));
        }

        let (left, right, ty, signed) = self.unify(left, lhs_ty, right, rhs_ty)?;
        if ty.is_float() {
            return Ok(Some(match op {
                IrBinOp::Add => self.builder.ins().fadd(left, right),
                IrBinOp::Sub => self.builder.ins().fsub(left, right),
                IrBinOp::Mul => self.builder.ins().fmul(left, right),
                IrBinOp::Div => self.builder.ins().fdiv(left, right),
                // No instruction: `%` on floating point is IEEE's remainder truncated toward
                // zero, which the runtime computes exactly on the significands.
                IrBinOp::Rem => {
                    let suffix = if ty == types::F32 { "float" } else { "double" };
                    let operand = if ty == types::F32 {
                        Ty::Float
                    } else {
                        Ty::Double
                    };
                    let Some(value) = self.runtime_call(
                        &format!("kt_rem_{suffix}"),
                        &[operand, operand],
                        operand,
                        &[left, right],
                    )?
                    else {
                        return Err("a `%` on floating point that yields no value".to_string());
                    };
                    value
                }
                IrBinOp::Lt
                | IrBinOp::Le
                | IrBinOp::Gt
                | IrBinOp::Ge
                | IrBinOp::Eq
                | IrBinOp::Ne => {
                    let condition = float_comparison(op).expect("comparison");
                    self.builder.ins().fcmp(condition, left, right)
                }
                other => return Err(format!("`{other:?}` on floating-point operands")),
            }));
        }

        // Arithmetic on the narrow types is `Int` arithmetic; `Boolean` is not narrow arithmetic.
        let arithmetic = !matches!(
            op,
            IrBinOp::Lt
                | IrBinOp::Le
                | IrBinOp::Gt
                | IrBinOp::Ge
                | IrBinOp::Eq
                | IrBinOp::Ne
                | IrBinOp::And
                | IrBinOp::Or
        );
        let (left, right, ty) = if arithmetic && ty.bits() < 32 && lhs_ty != Some(Ty::Boolean) {
            (
                self.widen_narrow_integer(left, lhs_ty),
                self.widen_narrow_integer(right, rhs_ty),
                types::I32,
            )
        } else {
            (left, right, ty)
        };

        // `Char + Int` is `Char`, and the arithmetic above ran at `Int` width, so the result is
        // narrowed back — the same `i2c` kotlinc emits after the `iadd`. Everything else keeps the
        // width it was computed at.
        let narrow_to_char =
            arithmetic_result(op, lhs_ty.map_or(Ty::Int, |ty| ty.non_null()), rhs_ty) == Ty::Char
                && lhs_ty.map(Ty::non_null) == Some(Ty::Char);

        let value = match op {
            // `iadd`/`isub`/`imul` wrap, which is Kotlin's rule; there is nothing to guard.
            IrBinOp::Add => self.builder.ins().iadd(left, right),
            IrBinOp::Sub => self.builder.ins().isub(left, right),
            IrBinOp::Mul => self.builder.ins().imul(left, right),
            // Division by zero throws and `MIN_VALUE / -1` wraps in Kotlin; the machine traps on
            // both, so the runtime decides.
            IrBinOp::Div | IrBinOp::Rem => {
                let (name, kotlin) = if ty == types::I64 {
                    ("long", Ty::Long)
                } else {
                    ("int", Ty::Int)
                };
                let helper = if op == IrBinOp::Div { "div" } else { "rem" };
                let result = self.runtime_call(
                    &format!("kt_{helper}_{name}"),
                    &[kotlin, kotlin],
                    kotlin,
                    &[left, right],
                )?;
                result.expect("division returns a value")
            }
            IrBinOp::BitAnd | IrBinOp::And => self.builder.ins().band(left, right),
            IrBinOp::BitOr | IrBinOp::Or => self.builder.ins().bor(left, right),
            IrBinOp::BitXor => self.builder.ins().bxor(left, right),
            IrBinOp::Lt | IrBinOp::Le | IrBinOp::Gt | IrBinOp::Ge | IrBinOp::Eq | IrBinOp::Ne => {
                let condition = comparison(op, signed).expect("comparison");
                self.builder.ins().icmp(condition, left, right)
            }
            IrBinOp::RefEq | IrBinOp::RefNe | IrBinOp::Shl | IrBinOp::Shr | IrBinOp::Ushr => {
                unreachable!("handled above")
            }
        };
        Ok(Some(if narrow_to_char {
            self.builder.ins().ireduce(types::I16, value)
        } else {
            value
        }))
    }

    /// `Byte`/`Short`/`Char` operands of arithmetic become `Int`, as Kotlin's operators declare.
    fn widen_narrow_integer(&mut self, value: Value, ty: Option<Ty>) -> Value {
        let from = self.builder.func.dfg.value_type(value);
        if from.is_float() || from.bits() >= 32 {
            return value;
        }
        self.resize(value, from, !matches!(ty, Some(Ty::Char)), types::I32)
    }

    /// Unary minus. `-Int.MIN_VALUE` is `Int.MIN_VALUE` in Kotlin, and `ineg` wraps the same way.
    pub(super) fn negate(&mut self, operand: u32, ty: Ty) -> Result<Option<Value>, Unsupported> {
        let Some(value) = self.coerce(operand, ty)? else {
            return Err("a negation of `Unit`".to_string());
        };
        if self.terminated {
            return Ok(None);
        }
        Ok(Some(if carrier(ty).clif().is_some_and(Type::is_float) {
            self.builder.ins().fneg(value)
        } else {
            self.builder.ins().ineg(value)
        }))
    }

    /// `isNaN`, `isInfinite`, `isFinite` — each one comparison on the unboxed value.
    ///
    /// `NaN` is the only value not equal to itself, and `|x| < +infinity` is false for both an
    /// infinity and a `NaN`, which is exactly what "finite" excludes. Neither needs a bit pattern
    /// written down here.
    pub(super) fn float_predicate(
        &mut self,
        predicate: super::super::super::intrinsics::FloatPredicate,
        receiver: u32,
    ) -> Result<Option<Value>, Unsupported> {
        use super::super::super::intrinsics::FloatPredicate;
        let ty = match self.type_of(receiver).map(Ty::non_null) {
            Some(Ty::Double) => Ty::Double,
            Some(Ty::Float) => Ty::Float,
            _ => return Err("a floating-point question about a value of another type".to_string()),
        };
        let Some(value) = self.coerce(receiver, ty)? else {
            return Err("a floating-point question about `Unit`".to_string());
        };
        if self.terminated {
            return Ok(None);
        }
        Ok(Some(match predicate {
            FloatPredicate::IsNaN => self.builder.ins().fcmp(FloatCC::NotEqual, value, value),
            other => {
                let magnitude = self.builder.ins().fabs(value);
                let infinity = if ty == Ty::Float {
                    self.builder.ins().f32const(f32::INFINITY)
                } else {
                    self.builder.ins().f64const(f64::INFINITY)
                };
                let condition = if other == FloatPredicate::IsFinite {
                    FloatCC::LessThan
                } else {
                    FloatCC::Equal
                };
                self.builder.ins().fcmp(condition, magnitude, infinity)
            }
        }))
    }
}

pub(super) fn scalar_bound(ty: Ty) -> Option<Ty> {
    let mut at = ty.non_null();
    for _ in 0..16 {
        if carrier(at) != Carrier::Ref {
            return Some(at);
        }
        match at {
            Ty::TyParam(_, bound) => at = bound.non_null(),
            _ => return None,
        }
    }
    None
}

/// Whether a type is one Kotlin compares by IEEE rules under `ProperIeee754Comparisons`: the
/// floating-point type itself, at either nullability and through a type parameter bounded by it.
///
/// A supertype is NOT one — `Any` holding a `Double` compares by `equals`, which is the total
/// order — so the bound is followed only while it stays floating-point.
fn is_ieee_operand(ty: Ty) -> bool {
    match ty {
        Ty::Double | Ty::Float => true,
        Ty::Nullable(inner) | Ty::PlatformNullable(inner) => is_ieee_operand(*inner),
        Ty::TyParam(_, bound) => is_ieee_operand(*bound),
        _ => false,
    }
}

/// The integer condition for a Kotlin comparison operator.
fn comparison(op: IrBinOp, signed: bool) -> Option<IntCC> {
    Some(match (op, signed) {
        (IrBinOp::Eq | IrBinOp::RefEq, _) => IntCC::Equal,
        (IrBinOp::Ne | IrBinOp::RefNe, _) => IntCC::NotEqual,
        (IrBinOp::Lt, true) => IntCC::SignedLessThan,
        (IrBinOp::Le, true) => IntCC::SignedLessThanOrEqual,
        (IrBinOp::Gt, true) => IntCC::SignedGreaterThan,
        (IrBinOp::Ge, true) => IntCC::SignedGreaterThanOrEqual,
        (IrBinOp::Lt, false) => IntCC::UnsignedLessThan,
        (IrBinOp::Le, false) => IntCC::UnsignedLessThanOrEqual,
        (IrBinOp::Gt, false) => IntCC::UnsignedGreaterThan,
        (IrBinOp::Ge, false) => IntCC::UnsignedGreaterThanOrEqual,
        _ => return None,
    })
}

/// The floating-point condition for a Kotlin comparison: ordered for `<`/`<=`/`>`/`>=`/`==` (any
/// NaN makes them false) and unordered for `!=` (`NaN != NaN` is true), as IEEE and Kotlin agree.
fn float_comparison(op: IrBinOp) -> Option<FloatCC> {
    Some(match op {
        IrBinOp::Eq => FloatCC::Equal,
        IrBinOp::Ne => FloatCC::NotEqual,
        IrBinOp::Lt => FloatCC::LessThan,
        IrBinOp::Le => FloatCC::LessThanOrEqual,
        IrBinOp::Gt => FloatCC::GreaterThan,
        IrBinOp::Ge => FloatCC::GreaterThanOrEqual,
        _ => return None,
    })
}

/// The type Kotlin performs an operation IN, which is not always either operand's.
pub(super) fn arithmetic_result(op: IrBinOp, lhs: Ty, rhs: Option<Ty>) -> Ty {
    match (lhs, op) {
        (Ty::Char, IrBinOp::Add) => Ty::Char,
        (Ty::Char, IrBinOp::Sub) if rhs.map(Ty::non_null) != Some(Ty::Char) => Ty::Char,
        (Ty::Byte | Ty::Short | Ty::Char, _) => Ty::Int,
        (other, _) => other,
    }
}
