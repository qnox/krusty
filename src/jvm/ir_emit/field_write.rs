//! JVM realization of checked instance-field writes.

use super::{
    emit_num_conv, instance_field_jvm_name, jvm_declared_ty, load, slot_words, static_storage,
    type_descriptor, Emitter,
};
use crate::ir::{ClassId, IrBinOp, IrExpr};
use crate::jvm::classfile::CodeBuilder;
use crate::types::Ty;

impl Emitter<'_> {
    pub(super) fn emit_set_field(
        &mut self,
        statement: u32,
        receiver: u32,
        class: ClassId,
        index: u32,
        value: u32,
        code: &mut CodeBuilder,
    ) {
        if self.diverges(receiver) {
            self.emit_value(receiver, code);
            return;
        }
        let class_decl = &self.ir.classes[class as usize];
        let field = &class_decl.fields[index as usize];
        let name = instance_field_jvm_name(self.ir, class_decl, field);
        let field_ty = jvm_declared_ty(&field.ty);
        let owner = class_decl.fq_name();
        if static_storage(self.ir, class_decl) {
            self.emit_static_storage_field(receiver, value, &owner, &name, field_ty, code);
            return;
        }
        if self.emit_int_self_sub(receiver, class, index, value, code) {
            return;
        }
        if self.diverges(value) {
            // Kotlin still evaluates the receiver before the RHS. A branchy divergent RHS must
            // start from a clean operand stack, so preserve an effectful receiver through a
            // temporary before emitting the non-returning value.
            if self.emits_control_flow(value) {
                let temps = self.spill_to_temps(&[receiver], code);
                self.emit_value(value, code);
                self.release_temporary(temps[0].2);
            } else {
                self.emit_value(receiver, code);
                self.emit_value(value, code);
            }
            return;
        }
        // A branchy value emits a merge frame; spill it before loading the receiver so those frames
        // begin with a clean operand stack. Plain values retain direct receiver/value order.
        if self.emits_control_flow(value) {
            let temps = self.spill_to_temps(&[value], code);
            self.emit_value(receiver, code);
            let (slot, ty, lease) = temps[0];
            load(ty, slot, code);
            self.release_temporary(lease);
        } else {
            self.emit_value(receiver, code);
            self.emit_value(value, code);
        }
        self.coerce_reference_on_stack(self.value_ty(value), field_ty, code);
        // A value that carried its OWN source line leaves that line in effect; the store belongs to
        // the statement, so kotlinc marks the statement's line again at the `putfield`. Without it
        // the property's line stays in effect over everything that follows the store.
        if let (Some(&statement_line), true) = (
            self.ir.expr_lines.get(&statement),
            self.ir.expr_source_lines.contains_key(&value),
        ) {
            code.mark_line(statement_line);
        }
        let field_ref = self.cw.fieldref(&owner, &name, &type_descriptor(field_ty));
        code.putfield(field_ref, slot_words(field_ty) as i32);
    }

    fn emit_static_storage_field(
        &mut self,
        receiver: u32,
        value: u32,
        owner: &str,
        name: &str,
        field_ty: Ty,
        code: &mut CodeBuilder,
    ) {
        // A static-storage object field has no instance operand. Evaluate a non-trivial receiver only
        // for its effects; the value then runs on a clean stack.
        if !matches!(self.ir.expr(receiver), IrExpr::GetValue(_)) {
            self.emit_value(receiver, code);
            code.pop();
        }
        self.emit_value(value, code);
        if self.diverges(value) {
            return;
        }
        self.coerce_reference_on_stack(self.value_ty(value), field_ty, code);
        let field_ref = self.cw.fieldref(owner, name, &type_descriptor(field_ty));
        code.putstatic(field_ref, slot_words(field_ty) as i32);
    }

    /// Emit `receiver.field = receiver.field - rhs` with one receiver load when both receivers are
    /// the same pure local. `dup` is a JVM representation choice over already-checked IR; common IR
    /// does not need a target-stack operation.
    fn emit_int_self_sub(
        &mut self,
        receiver: u32,
        class: ClassId,
        index: u32,
        value: u32,
        code: &mut CodeBuilder,
    ) -> bool {
        let rhs = match self.ir.expr(value) {
            IrExpr::PrimitiveBinOp {
                op: IrBinOp::Sub,
                lhs,
                rhs,
            } => match (self.ir.expr(receiver), self.ir.expr(*lhs)) {
                (
                    IrExpr::GetValue(write_receiver),
                    IrExpr::GetField {
                        receiver: read,
                        class: read_class,
                        index: read_index,
                    },
                ) if *read_class == class
                    && *read_index == index
                    && matches!(self.ir.expr(*read), IrExpr::GetValue(read_receiver) if read_receiver == write_receiver) =>
                {
                    *rhs
                }
                _ => return false,
            },
            _ => return false,
        };
        let class_decl = &self.ir.classes[class as usize];
        let field = &class_decl.fields[index as usize];
        let field_ty = jvm_declared_ty(&field.ty);
        if field_ty != Ty::Int || self.emits_control_flow(rhs) || self.must_spill_across(rhs) {
            return false;
        }
        let owner = class_decl.fq_name();
        let name = instance_field_jvm_name(self.ir, class_decl, field);

        self.emit_value(receiver, code);
        code.dup();
        let field_ref = self.cw.fieldref(&owner, &name, &type_descriptor(field_ty));
        code.getfield(field_ref, 1);
        self.emit_value(rhs, code);
        emit_num_conv(self.value_ty(rhs), Ty::Int, code);
        code.isub();
        code.putfield(field_ref, 1);
        true
    }
}
