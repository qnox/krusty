//! JVM realization of the generated-property equality and hash operations: a data class's
//! properties, and a value class's sole one.

use super::*;
use crate::ir::ExprId;

impl Emitter<'_> {
    /// Emit a generated property's contribution to `hashCode`: the value class's `hashCode-impl`,
    /// `Arrays.hashCode`, a boxed scalar's static `hashCode`, or the reference's own `hashCode()`.
    pub(super) fn emit_generated_property_hash(
        &mut self,
        ty: Ty,
        value: ExprId,
        code: &mut CodeBuilder,
    ) {
        if self.emit_value_class_property_hash(ty, value, code) {
            return;
        }
        if ty.non_null().is_array() {
            self.emit_value(value, code);
            let method = crate::jvm::array_representation::arrays_hash_code(self.cw, ty);
            code.invokestatic(method, 1, 1);
        } else if ty.non_null().is_jvm_scalar() && !ty.is_nullable() {
            let scalar = ty.non_null();
            self.emit_value(value, code);
            let (owner, descriptor) = match scalar {
                Ty::Int => ("java/lang/Integer", "(I)I"),
                Ty::Short => ("java/lang/Short", "(S)I"),
                Ty::Byte => ("java/lang/Byte", "(B)I"),
                Ty::Char => ("java/lang/Character", "(C)I"),
                Ty::Boolean => ("java/lang/Boolean", "(Z)I"),
                Ty::Long => ("java/lang/Long", "(J)I"),
                Ty::Double => ("java/lang/Double", "(D)I"),
                Ty::Float => ("java/lang/Float", "(F)I"),
                _ => unreachable!("scalar data hash"),
            };
            let method = self.cw.methodref(owner, "hashCode", descriptor);
            code.invokestatic(method, slot_words(scalar) as i32, 1);
        } else {
            self.emit_value(value, code);
            let owner = generated_property_hash_owner(self.ir, self.bodies, ty)
                .expect("a checked reference property has a JVM hash owner");
            let method = self.cw.methodref(&owner, "hashCode", "()I");
            code.invokevirtual(method, 0, 1);
        }
    }

    /// Emit `equals-impl0` for an unboxed value-class field. A boxed nullable field deliberately
    /// declines so the caller compares the two boxes through ordinary reference equality semantics.
    pub(super) fn emit_value_class_property_equals(
        &mut self,
        declared: Ty,
        left: ExprId,
        right: ExprId,
        code: &mut CodeBuilder,
    ) -> bool {
        let Some((owner, underlying, false)) = self.value_class_property_operand(declared, left)
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
    pub(super) fn emit_value_class_property_hash(
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
        let Some((owner, underlying, boxed)) = self.value_class_property_operand(declared, value)
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

    /// Exact semantic identity, JVM carrier, and the already-selected JVM
    /// representation of one generated-property operation.
    fn value_class_property_operand(
        &self,
        declared: Ty,
        value: ExprId,
    ) -> Option<(TypeName, Ty, bool)> {
        let owner = declared.non_null().obj_internal()?;
        let underlying = crate::jvm::value_classes::boxed_value_class_carrier(self.ir, owner)?;
        let boxed = self.value_ty(value).non_null().obj_internal() == Some(owner);
        Some((owner, underlying, boxed))
    }
}

/// JVM dispatch owner for a generated property's reference `hashCode` call. Common IR carries only
/// the declared Kotlin type; interface dispatch and boxed scalar ownership are representation facts
/// derived here by the backend. `None` means the classfile seeder can use its primitive/array rule.
fn generated_property_hash_owner(ir: &IrFile, bodies: &dyn MethodBodies, ty: Ty) -> Option<String> {
    if ty.is_array() || (ty.non_null().is_jvm_scalar() && !ty.is_nullable()) {
        return None;
    }
    if let Some(owner) = ty.non_null().obj_internal() {
        if crate::jvm::value_classes::is_boxed_value_class(ir, owner) {
            return Some(owner.render());
        }
    }
    let mut owner = if ty.is_nullable() && ty.non_null().is_jvm_scalar() {
        "java/lang/Object".to_owned()
    } else {
        crate::jvm::names::instanceof_internal_name(ty.non_null())
    };
    if ty
        .non_null()
        .obj_internal()
        .and_then(|name| ir.class_id_by_name(name))
        .is_some_and(|class| ir.classes[class as usize].is_interface)
        || bodies.owner_is_interface(&owner)
    {
        owner = "java/lang/Object".to_owned();
    }
    Some(owner)
}
