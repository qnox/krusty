//! JVM realization of checked instance-field writes.

use super::{
    emit_num_conv, instance_field_jvm_name, jvm_value_ty, slot_words, static_storage,
    type_descriptor, Emitter,
};
use crate::ir::{ClassId, IrBinOp, IrExpr};
use crate::jvm::classfile::CodeBuilder;
use crate::jvm::constructor_debug::property_store_line;
use crate::types::Ty;

struct StoredConstructorProperty {
    class_name: String,
    field_name: String,
    ty: Ty,
}

impl Emitter<'_> {
    /// A constructor-property parameter keeps its source value slot in common IR. After that
    /// parameter has been stored, an initializer reads the field, which is what the JVM verifier
    /// and kotlinc both see. Plain parameters and reads before the store stay on the local slot.
    pub(super) fn load_constructor_value(
        &mut self,
        index: u32,
        ty: Ty,
        slot: u16,
        code: &mut CodeBuilder,
    ) {
        let Some(stored) = self.stored_constructor_property(index) else {
            super::load(ty, slot, code);
            return;
        };
        // A JVM instance constructor's receiver is the mandated physical slot 0. The active
        // constructor-class identity above proves this is not a generated static holder.
        code.aload(0);
        let descriptor = type_descriptor(stored.ty);
        let field = self
            .cw
            .fieldref(&stored.class_name, &stored.field_name, &descriptor);
        code.getfield(field, slot_words(stored.ty) as i32);
    }

    fn stored_constructor_property(&self, index: u32) -> Option<StoredConstructorProperty> {
        let class_id = self.constructor_initializer_class?;
        if index == 0 {
            return None;
        }
        let class = self.ir.classes.get(class_id as usize)?;
        let argument = class.ctor_args.get(index as usize - 1)?;
        if !argument.is_field {
            return None;
        }
        let field_index = argument.field_index?;
        let field = class.fields.get(field_index as usize)?;
        Some(StoredConstructorProperty {
            class_name: class.fq_name(),
            field_name: instance_field_jvm_name(self.ir, class, field),
            ty: field.ty,
        })
    }

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
        let initializer_line = self
            .constructor_initializer_class
            .is_some()
            .then(|| {
                self.ir
                    .classes
                    .get(class as usize)
                    .and_then(|class| property_store_line(self.ir, class, index, statement))
            })
            .flatten();
        let class_decl = &self.ir.classes[class as usize];
        let field = &class_decl.fields[index as usize];
        let name = instance_field_jvm_name(self.ir, class_decl, field);
        let field_ty = jvm_value_ty(&field.ty);
        let owner = class_decl.fq_name();
        if static_storage(self.ir, class_decl) {
            self.emit_static_storage_field(receiver, value, &owner, &name, field_ty, code);
            return;
        }
        let cross_class_method = super::static_accessors::cross_class_backing_field_method(
            self.cw,
            self.ir,
            &self.facade,
            self.static_owner,
            class,
            index,
            true,
        );
        if cross_class_method.is_none()
            && self.emit_int_self_sub(receiver, class, index, value, code)
        {
            return;
        }
        if self.diverges(value) {
            // Kotlin still evaluates the receiver before the RHS. A divergent RHS that cannot
            // carry the operand stack must start from a clean one, so preserve an effectful
            // receiver through a temporary before emitting the non-returning value.
            if self.spills_operand_prefix(value) {
                let temps = self.spill_to_temps(&[receiver], code);
                self.emit_value(value, code);
                self.release_temporary(temps[0].2);
            } else {
                self.emit_value(receiver, code);
                self.emit_value(value, code);
            }
            return;
        }
        if let Some(line) = initializer_line {
            code.mark_line(line);
        }
        // Receiver, then value, as Kotlin evaluates them. A value that cannot carry the operand
        // stack has both evaluated into temporaries in that order and reloaded; every other value,
        // branchy or not, runs with the receiver left on the stack, as kotlinc does.
        self.emit_operands(&[receiver, value], code);
        self.coerce_reference_on_stack(self.value_ty(value), field_ty, code);
        // A value that carried its OWN source line leaves that line in effect; the store belongs to
        // the statement, so kotlinc marks the statement's line again at the `putfield`. Without it
        // the property's line stays in effect over everything that follows the store.
        if self.ir.expr_source_lines.contains_key(&value) {
            if let Some(&statement_line) = self.ir.expr_lines.get(&statement) {
                self.mark_expression_line(statement, statement_line, code);
            }
        }
        if let Some(method) = cross_class_method {
            code.invokestatic(method, (1 + slot_words(field_ty)) as i32, 0);
        } else {
            let field_ref = self.cw.fieldref(&owner, &name, &type_descriptor(field_ty));
            code.putfield(field_ref, slot_words(field_ty) as i32);
        }
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
        let field_ty = jvm_value_ty(&field.ty);
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

/// Source lowering binds an effectful field-write receiver to a temporary before the write, so a
/// `SetField` whose receiver is itself a call is built directly here. The receiver still runs before
/// a value that needs a clean operand stack: its call sits ahead of the value's guarded range.
#[cfg(test)]
mod tests {
    use crate::ir::test_support::blank_class;
    use crate::ir::{Callee, IrCatch, IrConst, IrExpr, IrField, IrFile, IrFunction};
    use crate::jvm::classreader::read_method_code;
    use crate::jvm::ir_emit::invariant_tests::emit_for_test;
    use crate::jvm::ir_emit::EmitRun;
    use crate::types::{type_name, Ty};

    #[test]
    fn an_effectful_receiver_runs_before_a_value_that_spills() {
        let mut ir = IrFile::default();
        let mut node = blank_class("demo/Node");
        node.fields.push(IrField::new("count".into(), Ty::Int));
        let class = ir.add_class(node);
        let node_ty = Ty::obj("demo/Node");

        let read = ir.add_expr(IrExpr::GetValue(0));
        let passed = ir.add_expr(IrExpr::Return(Some(read)));
        let pass = ir.add_fun(IrFunction {
            name: "pass".into(),
            params: vec![node_ty],
            ret: node_ty,
            body: Some(passed),
            is_static: true,
            dispatch_receiver: None,
            param_checks: vec![],
        });

        let argument = ir.add_expr(IrExpr::GetValue(0));
        let receiver = ir.add_expr(IrExpr::Call {
            callee: Callee::Local(pass),
            dispatch_receiver: None,
            args: vec![argument],
        });
        let attempt = ir.add_expr(IrExpr::Const(IrConst::Int(1)));
        let recovered = ir.add_expr(IrExpr::Const(IrConst::Int(2)));
        let value = ir.add_expr(IrExpr::Try {
            body: attempt,
            catches: vec![IrCatch::generated(
                1,
                type_name("java/lang/Throwable"),
                recovered,
            )],
            finally: None,
            result: Ty::Int,
        });
        let write = ir.add_expr(IrExpr::SetField {
            receiver,
            class,
            index: 0,
            value,
        });
        let body = ir.add_expr(IrExpr::Block {
            stmts: vec![write],
            value: None,
        });
        ir.add_fun(IrFunction {
            name: "bump".into(),
            params: vec![node_ty],
            ret: Ty::Unit,
            body: Some(body),
            is_static: true,
            dispatch_receiver: None,
            param_checks: vec![],
        });

        let run = EmitRun::default();
        let classes = emit_for_test(&ir, "demo/FacadeKt", &run)
            .unwrap_or_else(|| panic!("emission bailed: {:?}", run.inline_bail()));
        let (_, facade) = classes
            .iter()
            .find(|(name, _)| name == "demo/FacadeKt")
            .expect("the facade class is emitted");
        let bump = read_method_code(facade, "bump", "(Ldemo/Node;)V").expect("bump has a body");
        const INVOKESTATIC: u8 = 0xb8;
        let receiver_call = bump
            .code
            .iter()
            .position(|&op| op == INVOKESTATIC)
            .expect("the receiver call is emitted");
        let guarded = bump.handlers.first().expect("the value keeps its handler");
        assert!(
            receiver_call < guarded.start_pc as usize,
            "receiver call at {receiver_call}, value guarded from {}",
            guarded.start_pc
        );
    }
}
