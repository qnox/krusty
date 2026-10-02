//! JVM `==`/`!=` of the unsigned value classes (`UByte`, `UShort`, `UInt`, `ULong`).
//!
//! Checked FIR keeps the comparison in source order. Each operand is then either the primitive
//! carrier (a parameter or a property) or the boxed class (`UInt?`, or a non-null value that
//! arrived through `FunctionN.invoke`). kotlinc's shape follows that pair:
//! - a carrier on the left and a box on the right calls `equals-impl(carrier, Object)`;
//! - a box on the left and a carrier on the right null-checks the box (`null` is not equal) and
//!   compares the unboxed carrier with the right operand;
//! - `!=` negates that result.
//!
//! Two carriers stay on the numeric comparison, and two boxes stay on `Intrinsics.areEqual`. An
//! operand that is not a variable or a constant is stored before the null check, in source order,
//! which the temporary elimination then folds the way kotlinc's does.

use super::*;
use crate::ir::IrBinOp;

/// Which mixed representation `==` this comparison is.
enum UnsignedMixed {
    /// `equals-impl(leftCarrier, rightBox)`.
    EqualsImpl { semantic: Ty },
    /// Null-safe unbox of the left box, then a carrier comparison with the right.
    UnboxLeft { semantic: Ty },
}

impl Emitter<'_> {
    /// Whether realizing this comparison branches before producing its Boolean. A carrier/box
    /// `equals-impl` of `==` is one call; `!=` negates it, and a boxed left operand null-checks.
    pub(super) fn unsigned_mixed_equality_branches(
        &self,
        op: IrBinOp,
        lhs: ExprId,
        rhs: ExprId,
    ) -> bool {
        match self.unsigned_mixed(lhs, rhs) {
            Some(UnsignedMixed::UnboxLeft { .. }) => true,
            Some(UnsignedMixed::EqualsImpl { .. }) => matches!(op, IrBinOp::Ne),
            None => false,
        }
    }

    /// Put the mixed unsigned comparison's Boolean on the stack. Returns whether it was one.
    pub(super) fn emit_unsigned_mixed_equality_value(
        &mut self,
        op: IrBinOp,
        lhs: ExprId,
        rhs: ExprId,
        code: &mut CodeBuilder,
    ) -> bool {
        let Some(shape) = self.unsigned_mixed(lhs, rhs) else {
            return false;
        };
        self.emit_unsigned_equal(lhs, rhs, &shape, code);
        if op == IrBinOp::Ne {
            self.negate_material_bool(code);
        }
        true
    }

    /// Branch to `target` when the mixed unsigned comparison's result equals `jt`.
    pub(super) fn emit_unsigned_mixed_equality_branch(
        &mut self,
        op: IrBinOp,
        lhs: ExprId,
        rhs: ExprId,
        target: Label,
        jt: bool,
        code: &mut CodeBuilder,
    ) -> bool {
        let Some(shape) = self.unsigned_mixed(lhs, rhs) else {
            return false;
        };
        match shape {
            // `equals-impl` already yields the `==` Boolean, so `!=` and the condition invert the
            // test instead of materializing a second one.
            UnsignedMixed::EqualsImpl { semantic } => {
                self.emit_unsigned_equals_impl(lhs, rhs, semantic, code);
                if (op == IrBinOp::Eq) == jt {
                    code.ifne(target);
                } else {
                    code.ifeq(target);
                }
            }
            UnsignedMixed::UnboxLeft { semantic } => {
                self.emit_unsigned_unbox_equality(lhs, rhs, semantic, code);
                if op == IrBinOp::Ne {
                    self.negate_material_bool(code);
                }
                if jt {
                    code.ifne(target);
                } else {
                    code.ifeq(target);
                }
            }
        }
        true
    }

    fn unsigned_mixed(&self, lhs: ExprId, rhs: ExprId) -> Option<UnsignedMixed> {
        let left = self.unsigned_operand(lhs)?;
        let right = self.unsigned_operand(rhs)?;
        if left.semantic != right.semantic {
            return None;
        }
        match (left.boxed, right.boxed) {
            (false, true) => Some(UnsignedMixed::EqualsImpl {
                semantic: left.semantic,
            }),
            (true, false) => Some(UnsignedMixed::UnboxLeft {
                semantic: left.semantic,
            }),
            (false, false) | (true, true) => None,
        }
    }

    /// The unsigned class of `expr`, and whether its JVM slot is the box rather than the carrier.
    fn unsigned_operand(&self, expr: ExprId) -> Option<UnsignedOperand> {
        let logical = self
            .ir
            .logical_types
            .get(&expr)
            .copied()
            .unwrap_or_else(|| self.value_ty(expr));
        let semantic = logical.non_null().canonical_semantic();
        if !semantic.is_unsigned() {
            return None;
        }
        Some(UnsignedOperand {
            semantic,
            boxed: !self.value_ty(expr).is_jvm_scalar(),
        })
    }

    fn emit_unsigned_equal(
        &mut self,
        lhs: ExprId,
        rhs: ExprId,
        shape: &UnsignedMixed,
        code: &mut CodeBuilder,
    ) {
        match *shape {
            UnsignedMixed::EqualsImpl { semantic } => {
                self.emit_unsigned_equals_impl(lhs, rhs, semantic, code);
            }
            UnsignedMixed::UnboxLeft { semantic } => {
                self.emit_unsigned_unbox_equality(lhs, rhs, semantic, code);
            }
        }
    }

    /// `Unsigned.equals-impl(carrier, Object)Z`. The carrier stays unboxed; the box is the reference.
    fn emit_unsigned_equals_impl(
        &mut self,
        lhs: ExprId,
        rhs: ExprId,
        semantic: Ty,
        code: &mut CodeBuilder,
    ) {
        let carrier = semantic
            .scalar_value_repr()
            .expect("an unsigned class has a primitive carrier");
        self.emit_operands(&[lhs, rhs], code);
        self.mark_comparison_decision(&[lhs, rhs], code);
        let owner = semantic
            .kotlin_class_internal()
            .expect("an unsigned class names its Kotlin classifier");
        let descriptor = format!("({}Ljava/lang/Object;)Z", type_descriptor(carrier));
        let method = self
            .cw
            .methodref(&owner.render(), "equals-impl", &descriptor);
        let words = i32::from(slot_words(carrier)) + 1;
        code.invokestatic(method, words, 1);
    }

    /// `if (box == null) false else unbox(box) == carrier`.
    fn emit_unsigned_unbox_equality(
        &mut self,
        lhs: ExprId,
        rhs: ExprId,
        semantic: Ty,
        code: &mut CodeBuilder,
    ) {
        let carrier = semantic
            .scalar_value_repr()
            .expect("an unsigned class has a primitive carrier");
        let right_stored = !Self::reloads_in_place(self.ir.expr(rhs));
        let left_stored = right_stored && !Self::reloads_in_place(self.ir.expr(lhs));
        let mut releases = Vec::new();
        let left_local = if left_stored {
            self.emit_value(lhs, code);
            let ty = self.value_ty(lhs);
            let (slot, _, lease) = self.spill_operand(lhs, ty, code);
            releases.push(lease);
            Some((slot, ty))
        } else {
            None
        };
        let right_local = if right_stored {
            self.emit_value(rhs, code);
            let (slot, _, lease) = self.spill_operand(rhs, carrier, code);
            releases.push(lease);
            Some(slot)
        } else {
            None
        };

        let base = code.stack_height().max(0);
        if let Some((slot, ty)) = left_local {
            load(ty, slot, code);
        } else {
            self.emit_value(lhs, code);
        }
        code.dup();
        let present = code.new_label();
        let unequal = code.new_label();
        let done = code.new_label();
        code.ifnonnull(present);
        code.pop();
        code.push_int(0, self.cw);
        code.goto(done);
        self.bind(present, code);
        code.set_stack(u16::try_from(base + 1).unwrap_or(u16::MAX));
        unbox_prim_from(self.cw, code, self.value_ty(lhs), semantic);
        if let Some(slot) = right_local {
            load(carrier, slot, code);
        } else {
            self.emit_value(rhs, code);
        }
        self.mark_comparison_decision(&[lhs, rhs], code);
        let height = code.stack_height();
        let wide = matches!(carrier, Ty::Long);
        if wide {
            code.lcmp();
            code.ifne(unequal);
        } else {
            code.if_icmpne(unequal);
        }
        code.push_int(1, self.cw);
        code.goto(done);
        self.bind(unequal, code);
        let compared = if wide { height - 4 } else { height - 2 };
        code.set_stack(u16::try_from(compared.max(0)).unwrap_or(0));
        code.push_int(0, self.cw);
        self.bind(done, code);
        for lease in releases.into_iter().rev() {
            self.release_temporary(lease);
        }
    }

    /// A variable or a constant can be evaluated again, so it does not need a temporary.
    fn reloads_in_place(expr: &IrExpr) -> bool {
        matches!(expr, IrExpr::GetValue(_) | IrExpr::Const(_))
    }
}

struct UnsignedOperand {
    semantic: Ty,
    boxed: bool,
}
