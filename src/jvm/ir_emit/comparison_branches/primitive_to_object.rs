//! kotlinc's `PrimitiveToObjectComparison`: structural `==` between an integral primitive (`int`
//! category or `long`) and a reference, or between a boxed primitive and such a primitive,
//! compared without boxing.
//!
//! The primitive is compared to the reference's unboxed value once the reference is known to be a
//! value of the primitive's box (`ifnonnull` on a box type, `instanceof` otherwise); any other
//! reference makes the equality false. The reverse form, `Object == primitive`, is not this
//! operation: an object's `equals` may be overridden.

use super::*;
use crate::ir::ExprId;

/// The physical operand shape kotlinc's `Equals` intrinsic selects this comparison for.
struct PrimitiveToObjectOperands {
    /// The source's left operand is the primitive one.
    left_is_primitive: bool,
    /// The integral primitive operand's type.
    primitive: Ty,
    /// The reference operand's physical type.
    reference: Ty,
}

impl Emitter<'_> {
    /// Branch on `lhs == rhs` (or `!=`) as `PrimitiveToObjectComparison`, when the operands have
    /// its shape. Returns `false` and emits nothing otherwise.
    pub(super) fn emit_primitive_to_object_branch(
        &mut self,
        op: IrBinOp,
        lhs: ExprId,
        rhs: ExprId,
        target: Label,
        jt: bool,
        code: &mut CodeBuilder,
    ) -> bool {
        let Some(operands) = self.primitive_to_object_operands(lhs, rhs) else {
            return false;
        };
        self.emit_operands(&[lhs, rhs], code);
        // `!=` is kotlinc's `Not` over the equality: its branches are the equality's, inverted.
        let jump_if_equal = (op == IrBinOp::Eq) == jt;
        self.mark_comparison_decision(&[lhs, rhs], code);
        if jump_if_equal {
            let wrong_type = code.new_label();
            self.check_type_and_unbox(&operands, wrong_type, code);
            primitive_equality_branch(operands.primitive, true, target, code);
            self.bind(wrong_type, code);
        } else {
            self.check_type_and_unbox(&operands, target, code);
            primitive_equality_branch(operands.primitive, false, target, code);
        }
        true
    }

    /// Whether `lhs == rhs` compares as `PrimitiveToObjectComparison`, a branch of its own.
    pub(super) fn is_primitive_to_object_comparison(&self, lhs: ExprId, rhs: ExprId) -> bool {
        self.primitive_to_object_operands(lhs, rhs).is_some()
    }

    /// Whether `PrimitiveToObjectComparison` stores the primitive: it does when that operand is
    /// on the right (see `check_type_and_unbox`).
    pub(super) fn primitive_to_object_stores(&self, lhs: ExprId, rhs: ExprId) -> bool {
        self.primitive_to_object_operands(lhs, rhs)
            .is_some_and(|operands| !operands.left_is_primitive)
    }

    /// The operand shape of `PrimitiveToObjectComparison`, from the operands' JVM types.
    fn primitive_to_object_operands(
        &self,
        lhs: ExprId,
        rhs: ExprId,
    ) -> Option<PrimitiveToObjectOperands> {
        let (left, right) = (self.value_ty(lhs), self.value_ty(rhs));
        // kotlinc's `AsmUtil.isIntOrLongPrimitive`: every JVM `int`-category type and `long`.
        let int_or_long =
            |ty: Ty| matches!(ty, Ty::Byte | Ty::Short | Ty::Char | Ty::Int | Ty::Long);
        if int_or_long(left) && !right.is_jvm_scalar() {
            return Some(PrimitiveToObjectOperands {
                left_is_primitive: true,
                primitive: left,
                reference: right,
            });
        }
        (int_or_long(right) && jvm_box_primitive(left).is_some()).then_some(
            PrimitiveToObjectOperands {
                left_is_primitive: false,
                primitive: right,
                reference: left,
            },
        )
    }

    /// With both operands on the stack, leave the two primitives to compare, or jump to
    /// `wrong_type` with both discarded when the reference is not a value of the primitive's box.
    ///
    /// A primitive on the right is stored first so that the reference is on top: kotlinc keeps it
    /// in a temporary rather than swapping, which its unboxing peephole passes rely on.
    fn check_type_and_unbox(
        &mut self,
        operands: &PrimitiveToObjectOperands,
        wrong_type: Label,
        code: &mut CodeBuilder,
    ) {
        let PrimitiveToObjectOperands {
            left_is_primitive,
            primitive,
            reference,
        } = *operands;
        let stored = (!left_is_primitive).then(|| {
            let temp = self.frame.enter_temp(TempRole::OperandSpill, primitive);
            let slot = temp.slot();
            store(primitive, slot, code);
            (slot, self.lease_frame_temporary(temp, primitive))
        });
        let height = code.stack_height().max(0);
        let compare = code.new_label();
        code.dup();
        if jvm_box_primitive(reference).is_some() {
            code.ifnonnull(compare);
        } else {
            let box_class = primitive
                .jvm_boxed_ref()
                .and_then(|boxed| boxed.obj_internal())
                .expect("an integral primitive has a JVM box");
            let class = self.cw.class_ref(&box_class.render());
            code.instance_of(class);
            code.ifne(compare);
        }
        // The reference is not comparable: neither value matters any more.
        code.pop();
        if left_is_primitive {
            discard(primitive, code);
        }
        code.goto(wrong_type);
        self.bind(compare, code);
        code.set_stack(u16::try_from(height).unwrap_or(u16::MAX));
        unbox_prim_from(self.cw, code, reference, primitive);
        if let Some((slot, lease)) = stored {
            load(primitive, slot, code);
            self.release_temporary(lease);
        }
    }
}

/// The primitive a JVM box type holds (`java/lang/Integer`, or a nullable primitive's box).
fn jvm_box_primitive(ty: Ty) -> Option<Ty> {
    match ty.nullable_primitive() {
        Some(primitive) => primitive.jvm_boxed_ref().map(|_| primitive),
        None => ty.non_null().unboxed_primitive(),
    }
}

/// Pop a primitive of type `ty` off the stack.
fn discard(ty: Ty, code: &mut CodeBuilder) {
    if slot_words(ty) == 2 {
        code.pop2();
    } else {
        code.pop();
    }
}

/// Branch to `target` when two `int`-category values or two `long`s on the stack are equal
/// (`equal`), or unequal.
fn primitive_equality_branch(ty: Ty, equal: bool, target: Label, code: &mut CodeBuilder) {
    let op = if equal { IrBinOp::Eq } else { IrBinOp::Ne };
    if ty == Ty::Long {
        code.lcmp();
        cmp0_branch(op, true, target, code);
    } else {
        icmp_branch(op, true, target, code);
    }
}
