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
        if self.carrier(ty) != Carrier::Ref {
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

    /// `==` and `!=` between two floating-point operands where at least one arrived BOXED.
    ///
    /// Kotlin's `ProperIeee754Comparisons` compares by IEEE rules whenever both static types are
    /// the floating-point type itself — through a type parameter bounded by it and through its
    /// nullable form — so `NaN == NaN` is false and `0.0 == -0.0` is true. `kt_equals` answers the
    /// TOTAL order instead, where those two verdicts are reversed; it becomes the right answer only
    /// once an operand widens to something like `Any`, which is why this cannot go there.
    ///
    /// `null` is not a floating-point value and never reaches the comparison: a reference operand
    /// is checked first, and a null equals only another null. A scalar operand cannot be null and
    /// is asked nothing.
    fn ieee_equality(
        &mut self,
        op: IrBinOp,
        lhs: u32,
        lhs_ty: Option<Ty>,
        rhs: u32,
        rhs_ty: Option<Ty>,
    ) -> Result<Option<Value>, Unsupported> {
        // The type each side unboxes to, which the guard has already established is a
        // floating-point one. Kotlin does not compare a `Double` with a `Float` through `==`, so
        // two different widths here are a shape this has no rule for rather than a conversion.
        let (Some(left_ty), Some(right_ty)) =
            (lhs_ty.and_then(scalar_bound), rhs_ty.and_then(scalar_bound))
        else {
            return Err(
                "an IEEE comparison whose operand names no floating-point type".to_string(),
            );
        };
        if left_ty != right_ty {
            return Err("an IEEE comparison between two floating-point widths".to_string());
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

        let merge = self.builder.create_block();
        self.builder.append_block_param(merge, types::I8);
        let compare = self.builder.create_block();
        let absent = self.builder.create_block();

        let left_null = self.is_null_operand(left, lhs_ty);
        let right_null = self.is_null_operand(right, rhs_ty);
        let either = self.builder.ins().bor(left_null, right_null);
        self.builder.ins().brif(either, absent, &[], compare, &[]);

        self.continue_in(absent);
        self.builder.seal_block(absent);
        let both = self.builder.ins().band(left_null, right_null);
        self.builder.ins().jump(merge, &[BlockArg::Value(both)]);

        self.continue_in(compare);
        self.builder.seal_block(compare);
        let left = self
            .convert(left, lhs_ty, left_ty)?
            .expect("a floating-point target yields a value");
        let right = self
            .convert(right, rhs_ty, right_ty)?
            .expect("a floating-point target yields a value");
        let equal = self.builder.ins().fcmp(FloatCC::Equal, left, right);
        self.builder.ins().jump(merge, &[BlockArg::Value(equal)]);

        self.continue_in(merge);
        self.builder.seal_block(merge);
        let answer = self.builder.block_params(merge)[0];
        Ok(Some(if op == IrBinOp::Ne {
            let one = self.builder.ins().iconst(types::I8, 1);
            self.builder.ins().bxor(answer, one)
        } else {
            answer
        }))
    }

    /// Whether an operand is `null`, as a `Boolean`. A scalar one never is, and says so with a
    /// constant rather than a comparison against a pointer it is not.
    fn is_null_operand(&mut self, value: Value, ty: Option<Ty>) -> Value {
        if ty.map(|ty| self.carrier(ty)) != Some(Carrier::Ref) {
            return self.builder.ins().iconst(types::I8, 0);
        }
        let zero = self.builder.ins().iconst(types::I64, 0);
        self.builder.ins().icmp(IntCC::Equal, value, zero)
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
            // Two occurrences of one value class, both carried as the value: Kotlin's `==` on
            // them is the class's `equals`, which compares the values by THEIR type's `equals` —
            // so `NaN` equals itself and the two zeroes differ, as they do in a box.
            let unboxed = |ty: Option<Ty>| {
                ty.filter(|ty| !ty.is_nullable())
                    .and_then(|ty| self.file.values.unboxed(ty))
            };
            if let (Some(left), Some(right)) = (unboxed(lhs_ty), unboxed(rhs_ty)) {
                // A class that declares its own `equals` answers by it, through its box below.
                if left == right && !self.declares_its_own_equals(left) {
                    let class = Ty::Obj(left, &[]);
                    let Some(lhs) = self.coerce(lhs, class)? else {
                        return Ok(None);
                    };
                    let Some(rhs) = self.coerce(rhs, class)? else {
                        return Ok(None);
                    };
                    if self.terminated {
                        return Ok(None);
                    }
                    let equal = self.values_equal(lhs, rhs, class)?;
                    return Ok(Some(if op == IrBinOp::Ne {
                        let one = self.builder.ins().iconst(types::I8, 1);
                        self.builder.ins().bxor(equal, one)
                    } else {
                        equal
                    }));
                }
            }
            let against_null = matches!(self.file.ir.expr(lhs), IrExpr::Const(IrConst::Null))
                || matches!(self.file.ir.expr(rhs), IrExpr::Const(IrConst::Null));
            // `Unit` counts as a reference here: it is a VALUE in Kotlin, the runtime owns the one
            // instance of it, and `reference` materializes that singleton for an operand that
            // produces no machine value. `println("x") == Unit` is true, and comparing the two
            // through the ordinary reference path is what says so.
            let reference_operand = |ty: Option<Ty>| {
                matches!(
                    ty.map(|ty| self.carrier(ty)),
                    Some(Carrier::Ref | Carrier::Void)
                )
            };
            // A value class answering `equals` itself is asked through its box, whatever it holds.
            let own_equals = [lhs_ty, rhs_ty]
                .into_iter()
                .filter_map(|ty| ty.and_then(|ty| self.file.values.unboxed(ty)))
                .any(|class| self.declares_its_own_equals(class));
            let on_references =
                reference_operand(lhs_ty) || reference_operand(rhs_ty) || own_equals;
            if against_null {
                // `x == null` is `x === null` in Kotlin: no `equals` is ever called.
                let left = self.reference(lhs)?;
                let right = self.reference(rhs)?;
                let condition = comparison(op, true).expect("equality");
                return Ok(Some(self.builder.ins().icmp(condition, left, right)));
            }
            // A function value is compared like any other reference: through `kt_equals` below,
            // which dispatches `equals` on the receiver's own table. The TABLE is what makes that
            // right, rather than anything this site can see. A callable reference carries
            // `kt_reference_equals`, keyed by the declaration it names and the receiver it binds,
            // so `Foo::bar == Foo::bar` is true though the two are different objects. A property
            // reference carries its own, comparing the type — one per property — and the bound
            // receivers. A lambda carries `kotlin.Any`'s, which is the identity Kotlin gives one.
            //
            // This site used to decline instead, because the static type does not say WHICH of the
            // three produced the value. It does not have to: the object does, at run time.
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
            // reference and the rule still applies, so the box is opened and the comparison made
            // on the numbers themselves.
            if on_references
                && [lhs_ty, rhs_ty]
                    .iter()
                    .all(|ty| ty.is_some_and(is_ieee_operand))
            {
                return self.ieee_equality(op, lhs, lhs_ty, rhs, rhs_ty);
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
            let both_scalars =
                matches!(lhs_ty.map(|ty| self.carrier(ty)), Some(Carrier::Scalar(..)))
                    && matches!(rhs_ty.map(|ty| self.carrier(ty)), Some(Carrier::Scalar(..)));
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
                // On FLOATING-POINT values it is the machine's comparison, not a bit comparison.
                // `===` between primitives is Kotlin's `==` — that is the whole of what the
                // deprecation warning says about it — so it carries IEEE's answers with it: `NaN`
                // is identical to nothing, itself included, and `-0.0` is identical to `0.0`. A bit
                // comparison would answer the opposite of both.
                if ty.is_float() {
                    let condition = float_comparison(match op {
                        IrBinOp::RefEq => IrBinOp::Eq,
                        _ => IrBinOp::Ne,
                    })
                    .expect("an equality");
                    return Ok(Some(self.builder.ins().fcmp(condition, left, right)));
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
        Ok(Some(
            if self.carrier(ty).clif().is_some_and(Type::is_float) {
                self.builder.ins().fneg(value)
            } else {
                self.builder.ins().ineg(value)
            },
        ))
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

impl BodyLowering<'_, '_, '_> {
    /// Whether a value class of this file declares its own `equals`, which its box's first vtable
    /// entry records: the synthesized answer by the value is there only where it does not.
    fn declares_its_own_equals(&self, classifier: TypeName) -> bool {
        self.file
            .ir
            .class_id_by_name(classifier)
            .is_some_and(|class| {
                !matches!(
                    self.file.model.layout(class).vtable.first(),
                    Some(model::Slot::ValueMember { .. })
                )
            })
    }
}

pub(super) fn scalar_bound(ty: Ty) -> Option<Ty> {
    let mut at = ty.non_null();
    for _ in 0..16 {
        // The MACHINE rule, not the projected one: a value class is no operand of a built-in
        // operator, whatever it wraps.
        if machine_carrier(at) != Carrier::Ref {
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

impl BodyLowering<'_, '_, '_> {
    /// One of `kotlin.experimental`'s bit operations on a `Byte` or a `Short`.
    ///
    /// Nothing is called: the answer is the machine's, at the receiver's own width, and Kotlin's
    /// own `Int` and `Long` members of the same four names are realized the same way a few lines
    /// up. What separates them is where the library put the declaration, not what it means.
    ///
    /// The width comes from the RESULT rather than from the receiver: `Byte.and(Byte)` answers a
    /// `Byte`, so the declaration already states the width all three operands share, and reading
    /// it there needs no guess about what the receiver arrived carried as.
    pub(super) fn experimental_bitwise(
        &mut self,
        op: super::super::super::intrinsics::BitwiseOp,
        receiver: u32,
        args: &[u32],
        ret: Ty,
    ) -> Result<Option<Value>, Unsupported> {
        use super::super::super::intrinsics::BitwiseOp;
        let operand = ret.non_null();
        if !matches!(operand, Ty::Byte | Ty::Short) {
            return Err(format!(
                "a `kotlin.experimental` bit operation answering `{operand:?}`"
            ));
        }
        let Some(left) = self.coerce(receiver, operand)? else {
            return Err("a `Unit` receiver for a bit operation".to_string());
        };
        let right = match args {
            [] => None,
            [argument] => match self.coerce(*argument, operand)? {
                Some(value) => Some(value),
                None => return Err("a `Unit` operand for a bit operation".to_string()),
            },
            _ => return Err("a bit operation with more than one operand".to_string()),
        };
        if self.terminated {
            return Ok(None);
        }
        let produced = match (op, right) {
            (BitwiseOp::And, Some(right)) => self.builder.ins().band(left, right),
            (BitwiseOp::Or, Some(right)) => self.builder.ins().bor(left, right),
            (BitwiseOp::Xor, Some(right)) => self.builder.ins().bxor(left, right),
            (BitwiseOp::Inv, None) => self.builder.ins().bnot(left),
            _ => return Err("a bit operation of the wrong arity".to_string()),
        };
        self.convert(produced, Some(operand), ret)
    }
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

impl BodyLowering<'_, '_, '_> {
    /// A member of a PRIMITIVE asked by name, realized as the operation the declaration is.
    ///
    /// `a[i]` and `!b` reach the generator as compiler-supplied operations, because the frontend
    /// recognizes the source FORM and names one. `(IntArray::get)(a, i)` and `(Boolean::not)(b)`
    /// name the very same declarations through an ordinary dependency call, which the member path
    /// would otherwise look for a runtime entry point for and find none — a primitive has no
    /// methods to reach. The declaration is what says what the call means, not the spelling, so
    /// the answer here is the operation.
    ///
    /// Returns `None` when this is somebody else's member, so the caller falls through.
    pub(super) fn primitive_member(
        &mut self,
        owner: &str,
        name: &str,
        receiver: u32,
        args: &[u32],
        ret: Ty,
    ) -> Option<Result<Option<Value>, Unsupported>> {
        let ty = self.type_of(receiver)?.non_null();
        // The RECEIVER decides, not the owner: the eight primitive arrays and `Array<T>` are eight
        // owners naming one operation, and the receiver already distinguishes them for every other
        // array answer this generator gives.
        if ty.is_array() {
            return match (name, args) {
                ("get", [index]) => Some(self.array_get(receiver, *index, ret)),
                ("set", [index, value]) => Some(self.array_set(receiver, *index, *value)),
                _ => None,
            };
        }
        if ty == Ty::Boolean
            && name == "not"
            && args.is_empty()
            && super::super::super::intrinsics::is_boolean_base(owner)
        {
            return Some(self.boolean_not(receiver, ret));
        }
        None
    }

    /// `!b`, with the operand read at its own width rather than through a box.
    fn boolean_not(&mut self, receiver: u32, ret: Ty) -> Result<Option<Value>, Unsupported> {
        let Some(value) = self.coerce(receiver, Ty::Boolean)? else {
            return Err("a `Unit` operand of `Boolean.not`".to_string());
        };
        if self.terminated {
            return Ok(None);
        }
        let negated = self.builder.ins().bxor_imm_u(value, 1);
        self.convert(negated, Some(Ty::Boolean), ret)
    }
}
