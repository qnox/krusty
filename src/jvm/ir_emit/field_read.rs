//! JVM realization of checked instance-field reads.

use super::{
    instance_field_jvm_name, jvm_value_ty, slot_words, static_accessors, static_storage,
    type_descriptor, Emitter,
};
use crate::ir::{ClassId, IrExpr};
use crate::jvm::classfile::CodeBuilder;

impl Emitter<'_> {
    pub(super) fn emit_get_field(
        &mut self,
        receiver: u32,
        class: ClassId,
        index: u32,
        code: &mut CodeBuilder,
    ) {
        let class_decl = &self.ir.classes[class as usize];
        let field = &class_decl.fields[index as usize];
        let source_name = field.name.clone();
        let name = instance_field_jvm_name(self.ir, self.run, class_decl, index as usize);
        let field_ty = jvm_value_ty(&field.ty);
        let owner = class_decl.fq_name();
        let is_lateinit = field.is_lateinit();
        let reader = (!self.export_private_calls)
            .then_some(self.static_owner)
            .flatten();
        let method = {
            let planned = self.run.static_accessor_plan.borrow();
            static_accessors::cross_class_backing_field_method(
                self.cw,
                self.ir,
                self.run,
                &self.facade,
                &planned,
                reader,
                class,
                index,
                false,
            )
        };
        if let Some(method) = method {
            self.emit_value(receiver, code);
            code.invokestatic(method, 1, slot_words(field_ty) as i32);
        } else if static_storage(self.ir, class_decl) {
            // A static-storage object field has no instance operand. Evaluate a non-trivial
            // receiver only for its effects.
            if !matches!(self.ir.expr(receiver), IrExpr::GetValue(_)) {
                self.emit_value(receiver, code);
                code.pop();
            }
            let field_ref = self.cw.fieldref(&owner, &name, &type_descriptor(field_ty));
            code.getstatic(field_ref, slot_words(field_ty) as i32);
        } else {
            self.emit_value(receiver, code);
            let field_ref = self.cw.fieldref(&owner, &name, &type_descriptor(field_ty));
            code.getfield(field_ref, slot_words(field_ty) as i32);
        }
        // A `lateinit var` read throws while the field is still null. The bridge above performs a
        // raw field read, so the use site keeps the same guard as a direct read.
        if is_lateinit {
            code.dup();
            let initialized = code.new_label();
            code.ifnonnull(initialized);
            code.push_string(&source_name, self.cw);
            let throw = self.cw.methodref(
                "kotlin/jvm/internal/Intrinsics",
                "throwUninitializedPropertyAccessException",
                "(Ljava/lang/String;)V",
            );
            code.invokestatic(throw, 1, 0);
            self.bind(initialized, code);
        }
    }
}
