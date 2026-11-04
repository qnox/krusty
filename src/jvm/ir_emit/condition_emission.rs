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
    /// Returns `true` when evaluating the condition has no fall-through: either a constant takes
    /// `target` unconditionally or the condition itself transfers control (`break`, `continue`,
    /// `return`, or `throw`). Whatever the caller would emit next is then dead code with no incoming
    /// stack-map state, so the caller must emit nothing on that path.
    #[must_use = "an unconditionally-taken jump makes the fall-through path dead — emitting there \
                  leaves frameless code the verifier rejects"]
    pub(super) fn emit_cond_branch(
        &mut self,
        cond: u32,
        target: Label,
        jump_when_true: bool,
        code: &mut CodeBuilder,
    ) -> bool {
        self.emitting(cond, |emitter| {
            emitter.emit_cond_branch_node(cond, target, jump_when_true, code)
        })
    }

    fn emit_cond_branch_node(
        &mut self,
        cond: u32,
        target: Label,
        jump_when_true: bool,
        code: &mut CodeBuilder,
    ) -> bool {
        let operand_line = self.short_circuit_operand_line.take();
        // A constant condition folds: `while (true)` (a `Boolean(true)` pre-test, jump-out-when-false)
        // emits NO branch — a spurious `ifeq end` to the method end leaves a branch target with no
        // stack-map frame. An always-taken branch becomes an unconditional `goto`.
        if let IrExpr::Const(IrConst::Boolean(b)) = *self.ir.expr(cond) {
            self.mark_boolean_constant(cond, code);
            // Only emit the jump when the constant actually takes it. Final-bytecode analysis
            // discovers the target state from the emitted control-flow edge.
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
        // kotlinc's `andand`/`oror` intrinsic marks the expression's own line before its first
        // operand, and each operand's line again once its value is computed, for the jump that
        // consumes it (`BooleanConjunction`/`BooleanDisjunction`). The operands are not branch
        // conditions of their own: an enclosing `when` condition's state carries through them.
        if let Some((first, second, kind)) = self.short_circuit_operands(cond) {
            self.short_circuit_operand_line = None;
            self.mark_kept_start_line(first, code);
            let decided_by = matches!(kind, crate::ir::IrShortCircuitKind::Or);
            if decided_by == jump_when_true {
                if self.emit_short_circuit_operand(first, target, jump_when_true, code) {
                    return true;
                }
                return self.emit_short_circuit_operand(second, target, jump_when_true, code);
            }
            let skip = code.new_label();
            if !self.emit_short_circuit_operand(first, skip, decided_by, code) {
                let _ = self.emit_short_circuit_operand(second, target, jump_when_true, code);
            }
            self.bind(skip, code);
            return false;
        }
        // A negation is only polarity: kotlinc's `Not` jumps on its operand with the opposite
        // sense. A Boolean `==`/`!=`, a literal operand included, is an ordinary comparison below,
        // which kotlinc's `BooleanComparison` materializes on both sides.
        if let Some(operand) = self.ir.negated_operand(cond) {
            self.short_circuit_operand_line = operand_line;
            return self.emit_cond_branch(operand, target, !jump_when_true, code);
        }
        // A non-null floating property in synthesized data-class `equals` uses the language's
        // data-class equality rule, whose JVM realization is `<Box>.compare(a, b) == 0`. Emit that
        // comparison directly into the consuming branch so no boxed values or intermediate Boolean
        // are created. Other field kinds retain the generic intrinsic realization below.
        if let IrExpr::Call {
            callee: call_callee,
            args,
            ..
        } = self.ir.expr(cond)
        {
            if let Callee::Intrinsic {
                operation: crate::ir::IrIntrinsic::GeneratedPropertyEquals { ty },
                ..
            } = &**call_callee
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
                    if jump_when_true {
                        code.ifeq(target);
                    } else {
                        code.ifne(target);
                    }
                    return false;
                }
                // An enum property compares by reference, as kotlinc's `Equals` compares any two
                // operands of which one has an enum type: an enum constant is a singleton.
                let [left, right] = args.as_slice() else {
                    unreachable!("data-class field equality has two operands")
                };
                let (left, right) = (*left, *right);
                if self.is_enum_value(left) || self.is_enum_value(right) {
                    self.emit_value(left, code);
                    self.emit_value(right, code);
                    if jump_when_true {
                        code.if_acmpeq(target);
                    } else {
                        code.if_acmpne(target);
                    }
                    return false;
                }
            }
        }
        if let IrExpr::PrimitiveBinOp { op, .. } | IrExpr::Equality { op, .. } = *self.ir.expr(cond)
        {
            use IrBinOp::*;
            if matches!(op, Lt | Le | Gt | Ge | Eq | Ne | RefEq | RefNe) {
                self.emit_comparison_branch(cond, target, jump_when_true, code);
                return false;
            }
        }
        // Fuse `x is T` / `x !is T` into `instanceof; if{ne,eq}` — no 0/1 boolean is materialized
        // (kotlinc's shape, e.g. a data class `equals`' `instanceof; ifne <ok>`). A primitive
        // target tests its wrapper class, as the unfused check does. A nullable target
        // materializes its null-accepting check as a value. Copy the operation out before naming
        // its class: the recorded primitive-array role borrows the file.
        let inst_fuse = match self.ir.expr(cond) {
            IrExpr::TypeOp {
                op: to @ (IrTypeOp::InstanceOf | IrTypeOp::NotInstanceOf),
                arg,
                type_operand,
            } if !type_operand.is_nullable() => Some((*to, *arg, *type_operand)),
            _ => None,
        };
        let inst_fuse = inst_fuse.map(|(to, arg, type_operand)| {
            let internal = self.type_operation_class_name(cond, type_operand);
            (to, arg, type_operand, internal)
        });
        if let Some((to, arg, type_operand, internal)) = inst_fuse {
            let arg = self.instance_check_operand(arg);
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
            self.emit_instance_check(&internal, type_operand, code);
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
        // A condition that always transfers (`while (break)`, `do { … } while (break)`) emits
        // the jump and no Boolean. Testing a missing 0/1 afterwards is an empty-stack `ifeq`.
        if self.diverges(cond) {
            self.emit_value(cond, code);
            return true;
        }
        self.emit_value(cond, code);
        if let Some(line) = operand_line.and_then(|operand| self.ir.source_line(operand)) {
            self.mark_expression_line(operand_line.unwrap_or(cond), line, code);
        }
        if jump_when_true {
            code.ifne(target);
        } else {
            code.ifeq(target);
        }
        false
    }

    /// Emit one operand of a source `&&`/`||`, its line marked again before its jump.
    fn emit_short_circuit_operand(
        &mut self,
        operand: u32,
        target: Label,
        jump_when_true: bool,
        code: &mut CodeBuilder,
    ) -> bool {
        self.short_circuit_operand_line = Some(operand);
        let diverges = self.emit_cond_branch(operand, target, jump_when_true, code);
        self.short_circuit_operand_line = None;
        diverges
    }

    /// Read the operands from a provenance-tagged generic `when`. The lowering shape is an IR
    /// invariant. A constant operand is fused like any other: kotlinc's `jumpIfFalse(false)` is a
    /// `goto`, so `a > 0 && false` jumps both ways to the shared `iconst_0` and the method passes
    /// leave `ifle L; L: iconst_0`, not an ordinary `when`'s two materialized branches.
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
        Some((*first, second, kind))
    }
}
