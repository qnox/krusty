//! JVM realization of data-class equality and hash operations over value-class fields.

use super::*;
use crate::ir::ExprId;

impl Emitter<'_> {
    /// Emit `equals-impl0` for an unboxed value-class field. A boxed nullable field deliberately
    /// declines so the caller compares the two boxes through ordinary reference equality semantics.
    pub(super) fn emit_data_class_value_equals(
        &mut self,
        declared: Ty,
        left: ExprId,
        right: ExprId,
        code: &mut CodeBuilder,
    ) -> bool {
        let Some((owner, underlying, false)) = self.data_class_value_class_field(declared, left)
        else {
            return false;
        };
        self.emit_value(left, code);
        self.emit_value(right, code);
        let physical = jvm_declared_ty(&underlying);
        let descriptor = method_descriptor(&[physical, physical], Ty::Boolean);
        let method = self
            .cw
            .methodref(&owner.render(), "equals-impl0", &descriptor);
        code.invokestatic(method, slot_words(physical) as i32 * 2, 1);
        true
    }

    /// Emit the value-class `hashCode-impl`, unboxing a nullable boxed field first. The carrier is
    /// the terminal exact underlying declaration, not a one-level sibling-file approximation.
    pub(super) fn emit_data_class_value_hash(
        &mut self,
        declared: Ty,
        value: ExprId,
        code: &mut CodeBuilder,
    ) -> bool {
        if let Some((owner, carrier)) = native_unsigned_impl_target(declared) {
            self.emit_value(value, code);
            let descriptor = method_descriptor(&[carrier], Ty::Int);
            let method = self
                .cw
                .methodref(&owner.render(), "hashCode-impl", &descriptor);
            code.invokestatic(method, slot_words(carrier) as i32, 1);
            return true;
        }
        let Some((owner, underlying, boxed)) = self.data_class_value_class_field(declared, value)
        else {
            return false;
        };
        self.emit_value(value, code);
        let physical = jvm_declared_ty(&underlying);
        if boxed {
            let descriptor = method_descriptor(&[], physical);
            let method = self
                .cw
                .methodref(&owner.render(), "unbox-impl", &descriptor);
            code.invokevirtual(method, 0, slot_words(physical) as i32);
        }
        let descriptor = method_descriptor(&[physical], Ty::Int);
        let method = self
            .cw
            .methodref(&owner.render(), "hashCode-impl", &descriptor);
        code.invokestatic(method, slot_words(physical) as i32, 1);
        true
    }

    /// Exact semantic identity, terminal declared underlying, and the already-selected JVM
    /// representation of one generated data-class field operation.
    fn data_class_value_class_field(
        &self,
        declared: Ty,
        value: ExprId,
    ) -> Option<(TypeName, Ty, bool)> {
        let owner = declared.non_null().obj_internal()?;
        let underlying =
            crate::jvm::value_classes::boxed_value_class_terminal_underlying(self.ir, owner)?;
        let boxed = self.value_ty(value).non_null().obj_internal() == Some(owner);
        Some((owner, underlying, boxed))
    }
}
