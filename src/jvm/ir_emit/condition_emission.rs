//! JVM control-flow emission for Boolean conditions and source short-circuit operators.

use super::*;

impl Emitter<'_> {
    /// Emit a source short circuit in value position, materializing its fused condition once.
    pub(super) fn emit_short_circuit_value(
        &mut self,
        expression: u32,
        code: &mut CodeBuilder,
    ) -> bool {
        if self.short_circuit_operands(expression).is_none() {
            return false;
        }
        let false_path = code.new_label();
        let _ = self.emit_cond_branch(expression, false_path, false, code);
        self.materialize_cmp_bool(false_path, code);
        true
    }

    /// Emit a conditional jump to `target`, taken exactly when `cond` evaluates to `jump_when_true`.
    /// When `cond` is a primitive/reference comparison it is FUSED into the branch (`if_icmpge`,
    /// `ifnull`, `if_acmpeq`, `lcmp;ifge`, …) instead of materializing a 0/1 boolean and testing it
    /// with `ifeq`/`ifne` — the bytecode kotlinc emits for every `if`/`while`/`for` over a comparison.
    ///
    /// Returns `true` when the jump was emitted UNCONDITIONALLY (a constant condition that always
    /// takes it): the caller's fall-through path is then statically unreachable, and whatever it would
    /// emit next lands after a `goto` with nothing branching to it — dead code with no stack-map frame,
    /// which the verifier rejects outright ("Expecting a stack map frame"). Such a caller must emit
    /// nothing on that path. kotlinc likewise emits no body for a never-entered branch.
    #[must_use = "an unconditionally-taken jump makes the fall-through path dead — emitting there \
                  leaves frameless code the verifier rejects"]
    pub(super) fn emit_cond_branch(
        &mut self,
        cond: u32,
        target: Label,
        jump_when_true: bool,
        code: &mut CodeBuilder,
    ) -> bool {
        // A constant condition folds: `while (true)` (a `Boolean(true)` pre-test, jump-out-when-false)
        // emits NO branch — a spurious `ifeq end` to the method end leaves a branch target with no
        // stack-map frame. An always-taken branch becomes an unconditional `goto`.
        if let IrExpr::Const(IrConst::Boolean(b)) = *self.ir.expr(cond) {
            // Frame the target regardless (callers — `when`/loop emission — rely on the branch target
            // having a stack-map frame), but only emit the jump when the constant actually takes it.
            self.frame(target, vec![], code);
            if b == jump_when_true {
                code.goto(target);
                return true;
            }
            return false;
        }
        // A source `&&`/`||` never materializes its Boolean in a condition. `&&` is decided by a
        // false left operand and `||` by a true one: that operand jumps on its own, and only the
        // right operand is left to decide the rest (kotlinc's `jumpIfFalse`/`jumpIfTrue` over
        // `ANDAND`/`OROR`). A hand-written `if (a) b else false` keeps the materialized form, as
        // kotlinc's does, which is why the lowering records which `when`s these are.
        if let Some((first, second, kind)) = self.short_circuit_operands(cond) {
            let decided_by = matches!(kind, crate::ir::IrShortCircuitKind::Or);
            if decided_by == jump_when_true {
                if self.emit_cond_branch(first, target, jump_when_true, code) {
                    return true;
                }
                return self.emit_cond_branch(second, target, jump_when_true, code);
            }
            let skip = code.new_label();
            if !self.emit_cond_branch(first, skip, decided_by, code) {
                let _ = self.emit_cond_branch(second, target, jump_when_true, code);
            }
            self.bind(skip, code);
            return false;
        }
        // Boolean equality against a literal is only polarity. Peel it before the general
        // comparison path so `(a == b) == false` branches directly on `a != b`, and an intrinsic
        // Boolean result is consumed by one `ifeq`/`ifne` rather than materialized and compared
        // again. This is a JVM realization optimization over already-checked common IR.
        if let IrExpr::PrimitiveBinOp { op, lhs, rhs } = *self.ir.expr(cond) {
            if matches!(op, IrBinOp::Eq | IrBinOp::Ne) {
                let literal = match (self.ir.expr(lhs), self.ir.expr(rhs)) {
                    (IrExpr::Const(IrConst::Boolean(value)), _)
                        if self.value_ty(rhs) == Ty::Boolean =>
                    {
                        Some((*value, rhs))
                    }
                    (_, IrExpr::Const(IrConst::Boolean(value)))
                        if self.value_ty(lhs) == Ty::Boolean =>
                    {
                        Some((*value, lhs))
                    }
                    _ => None,
                };
                if let Some((literal, operand)) = literal {
                    let operand_truth = if op == IrBinOp::Eq {
                        jump_when_true == literal
                    } else {
                        jump_when_true != literal
                    };
                    return self.emit_cond_branch(operand, target, operand_truth, code);
                }
            }
        }
        // A non-null floating property in synthesized data-class `equals` uses the language's
        // data-class equality rule, whose JVM realization is `<Box>.compare(a, b) == 0`. Emit that
        // comparison directly into the consuming branch so no boxed values or intermediate Boolean
        // are created. Other field kinds retain the generic intrinsic realization below.
        if let IrExpr::Call {
            callee:
                Callee::Intrinsic {
                    operation: crate::ir::IrIntrinsic::DataClassFieldEquals { ty },
                    ..
                },
            args,
            ..
        } = self.ir.expr(cond)
        {
            let scalar = ty.non_null();
            if !ty.is_nullable() && matches!(scalar, Ty::Float | Ty::Double) {
                let [left, right] = args.as_slice() else {
                    unreachable!("data-class field equality has two operands")
                };
                self.emit_value(*left, code);
                self.emit_value(*right, code);
                let (owner, descriptor) = match scalar {
                    Ty::Float => ("java/lang/Float", "(FF)I"),
                    Ty::Double => ("java/lang/Double", "(DD)I"),
                    _ => unreachable!(),
                };
                let method = self.cw.methodref(owner, "compare", descriptor);
                code.invokestatic(method, (slot_words(scalar) * 2) as i32, 1);
                self.frame(target, vec![], code);
                if jump_when_true {
                    code.ifeq(target);
                } else {
                    code.ifne(target);
                }
                return false;
            }
        }
        if let IrExpr::PrimitiveBinOp { op, lhs, rhs } = *self.ir.expr(cond) {
            use IrBinOp::*;
            if matches!(op, Lt | Le | Gt | Ge | Eq | Ne | RefEq | RefNe) {
                self.emit_compare_branch(op, lhs, rhs, target, jump_when_true, code);
                return false;
            }
        }
        // Fuse `x is T` / `x !is T` (a reference target) into `instanceof; if{ne,eq}` — no 0/1 boolean is
        // materialized (kotlinc's shape, e.g. a data class `equals`' `instanceof; ifne <ok>`).
        let inst_fuse = if let IrExpr::TypeOp {
            op: to,
            arg,
            type_operand,
        } = self.ir.expr(cond)
        {
            if matches!(to, IrTypeOp::InstanceOf | IrTypeOp::NotInstanceOf) {
                let jvm_ty = ir_ty_to_jvm(type_operand);
                (!jvm_ty.is_jvm_scalar()).then(|| {
                    (
                        *to,
                        *arg,
                        crate::jvm::names::instanceof_internal_name(jvm_ty),
                    )
                })
            } else {
                None
            }
        } else {
            None
        };
        if let Some((to, arg, internal)) = inst_fuse {
            let (physical_arg, semantic_arg) = self.emit_type_op_operand(arg, code);
            // `instanceof` takes a REFERENCE. A scalar operand is boxed first, exactly as the
            // unfused emit above does it — `if (n is Number)` where `n` is an `Int` reached here
            // and put an `int` where the verifier wants an object.
            if physical_arg.is_jvm_scalar() {
                box_prim_free(
                    self.cw,
                    code,
                    semantic_scalar_adapter(semantic_arg, physical_arg),
                );
            }
            let ci = self.cw.class_ref(&internal);
            code.instance_of(ci);
            self.frame(target, vec![], code);
            // Stack holds 1 iff `arg instanceof T`. The condition is true on `instanceof` for `InstanceOf`
            // and on `!instanceof` for `NotInstanceOf`; jump when the condition equals `jump_when_true`.
            let jump_on_instance = if matches!(to, IrTypeOp::InstanceOf) {
                jump_when_true
            } else {
                !jump_when_true
            };
            if jump_on_instance {
                code.ifne(target);
            } else {
                code.ifeq(target);
            }
            return false;
        }
        self.emit_value(cond, code);
        self.frame(target, vec![], code);
        if jump_when_true {
            code.ifne(target);
        } else {
            code.ifeq(target);
        }
        false
    }

    /// Read the operands from a provenance-tagged generic `when`. The lowering shape is an IR
    /// invariant; only constant operands deliberately stay on ordinary `when` emission, whose
    /// reachability handling already owns constant folding.
    fn short_circuit_operands(
        &self,
        expression: u32,
    ) -> Option<(u32, u32, crate::ir::IrShortCircuitKind)> {
        let kind = *self.ir.short_circuits.get(&expression)?;
        let IrExpr::When { branches } = self.ir.expr(expression) else {
            panic!("short-circuit provenance must identify an IrExpr::When");
        };
        let [(Some(first), first_result), (None, else_result)] = branches.as_slice() else {
            panic!("short-circuit provenance must retain two ordered branches");
        };
        let (second, sentinel, expected_sentinel) = match kind {
            crate::ir::IrShortCircuitKind::And => (*first_result, *else_result, false),
            crate::ir::IrShortCircuitKind::Or => (*else_result, *first_result, true),
        };
        let constant = |operand: u32| match self.ir.expr(operand) {
            IrExpr::Const(IrConst::Boolean(value)) => Some(*value),
            _ => None,
        };
        assert_eq!(
            constant(sentinel),
            Some(expected_sentinel),
            "short-circuit sentinel must agree with its recorded operator"
        );
        (constant(*first).is_none() && constant(second).is_none()).then_some((*first, second, kind))
    }
}
