//! JVM realization of common-IR type operations.
//!
//! Checked common IR fixes the semantic operation and target. This boundary owns wrapper classes,
//! erasure, null-check call shape, reload-versus-dup selection, casts, and numeric representation.

use crate::ir::{ExprId, IrBindingStability, IrExpr, IrTypeOp};
use crate::jvm::classfile::CodeBuilder;
use crate::types::{stored_value_ty, Ty};

use super::{
    box_prim_free, emit_num_conv, implicit_reference_coercion, ir_ty_to_jvm,
    semantic_scalar_adapter, type_descriptor, unbox_prim, Emitter,
};

impl Emitter<'_> {
    pub(super) fn emit_type_operation(
        &mut self,
        op: IrTypeOp,
        arg: ExprId,
        type_operand: Ty,
        code: &mut CodeBuilder,
    ) {
        // A primitive target of `instanceof`/`checkcast` (`x is Int`) tests the boxed wrapper.
        let jvm_ty = ir_ty_to_jvm(&type_operand);
        let internal = if jvm_ty.is_jvm_scalar() {
            semantic_scalar_adapter(type_operand, jvm_ty)
                .boxed_ref()
                .map(crate::jvm::names::instanceof_internal_name)
                .unwrap_or_else(|| crate::jvm::names::instanceof_internal_name(jvm_ty))
        } else {
            crate::jvm::names::instanceof_internal_name(jvm_ty)
        };
        crate::trace_compiler!(
            "value_classes",
            "emit type op={op:?} arg={arg} {:?} arg_ty={:?} operand={type_operand:?} jvm={jvm_ty:?} internal={internal}",
            self.ir.expr(arg),
            self.value_ty(arg),
        );
        if let IrExpr::Block { stmts, value } = self.ir.expr(arg) {
            crate::trace_compiler!(
                "value_classes",
                "type op block arg={arg} stmts={:?} value={:?}",
                stmts
                    .iter()
                    .map(|&expression| (expression, self.ir.expr(expression)))
                    .collect::<Vec<_>>(),
                value.map(|expression| (expression, self.ir.expr(expression))),
            );
        }
        let (physical_arg, semantic_arg) = self.emit_type_op_operand(arg, code);
        match op {
            IrTypeOp::InstanceOf => {
                if physical_arg.is_jvm_scalar() {
                    box_prim_free(
                        self.cw,
                        code,
                        semantic_scalar_adapter(semantic_arg, physical_arg),
                    );
                }
                let class = self.cw.class_ref(&internal);
                code.instance_of(class);
            }
            IrTypeOp::NotInstanceOf => {
                if physical_arg.is_jvm_scalar() {
                    box_prim_free(
                        self.cw,
                        code,
                        semantic_scalar_adapter(semantic_arg, physical_arg),
                    );
                }
                let class = self.cw.class_ref(&internal);
                code.instance_of(class);
                code.push_int(1, self.cw);
                code.ixor();
            }
            IrTypeOp::Cast => {
                // The emitter owns erasure: a `checkcast` to `java/lang/Object` (an unbounded
                // `as T`) is a no-op, and so is one whose target descriptor already equals the
                // value's physical descriptor.
                if physical_arg.is_jvm_scalar() {
                    box_prim_free(
                        self.cw,
                        code,
                        semantic_scalar_adapter(semantic_arg, physical_arg),
                    );
                }
                let redundant = if physical_arg.is_jvm_scalar() {
                    semantic_arg.non_null().boxed_ref().is_some_and(|source| {
                        crate::jvm::names::instanceof_internal_name(source) == internal
                    })
                } else {
                    type_descriptor(physical_arg) == type_descriptor(jvm_ty)
                };
                if internal != "java/lang/Object" && !redundant {
                    let class = self.cw.class_ref(&internal);
                    code.checkcast(class);
                }
            }
            IrTypeOp::CastNonNull => {
                self.emit_non_null_cast(
                    arg,
                    type_operand,
                    jvm_ty,
                    &internal,
                    physical_arg,
                    semantic_arg,
                    code,
                );
            }
            IrTypeOp::ImplicitCoercion => {
                self.emit_implicit_coercion(arg, type_operand, physical_arg, code);
            }
            IrTypeOp::SafeCast => {}
        }
    }

    fn emit_non_null_cast(
        &mut self,
        arg: ExprId,
        type_operand: Ty,
        jvm_ty: Ty,
        internal: &str,
        physical_arg: Ty,
        semantic_arg: Ty,
        code: &mut CodeBuilder,
    ) {
        let target_semantic = type_operand.non_null();
        let identical_scalar = physical_arg.is_jvm_scalar()
            && jvm_ty.is_jvm_scalar()
            && semantic_arg.non_null() == target_semantic;
        if identical_scalar {
            return;
        }

        // A source scalar first crosses the reference boundary as its own box. A cast to another
        // scalar must then fail the target wrapper check instead of becoming a numeric conversion.
        if physical_arg.is_jvm_scalar() {
            box_prim_free(
                self.cw,
                code,
                semantic_scalar_adapter(semantic_arg, physical_arg),
            );
        }
        let kotlin_name = match target_semantic {
            Ty::Obj(fq_name, _) => fq_name.render().replace('/', "."),
            Ty::TyParam(name, _) => crate::types::type_parameter_source_name(name).to_string(),
            _ => "kotlin.Any".to_string(),
        };
        let reread = !physical_arg.is_jvm_scalar()
            && self.ir.binding_read_stability.get(&arg) == Some(&IrBindingStability::Stable)
            && matches!(self.ir.expr(arg), IrExpr::GetValue(_));
        if !reread {
            code.dup();
        }
        code.push_string(
            &format!("null cannot be cast to non-null type {kotlin_name}"),
            self.cw,
        );
        let check = self.cw.methodref(
            "kotlin/jvm/internal/Intrinsics",
            "checkNotNull",
            "(Ljava/lang/Object;Ljava/lang/String;)V",
        );
        code.invokestatic(check, 2, 0);
        if reread {
            self.emit_type_op_operand(arg, code);
        }
        if internal != "java/lang/Object" {
            let class = self.cw.class_ref(internal);
            code.checkcast(class);
        }
        if jvm_ty.is_jvm_scalar() {
            unbox_prim(
                self.cw,
                code,
                semantic_scalar_adapter(target_semantic, jvm_ty),
            );
        }
    }

    fn emit_implicit_coercion(
        &mut self,
        arg: ExprId,
        type_operand: Ty,
        physical_arg: Ty,
        code: &mut CodeBuilder,
    ) {
        let target = ir_ty_to_jvm(&stored_value_ty(type_operand));
        crate::trace_compiler!(
            "value_classes",
            "coerce at={physical_arg:?} target={target:?} type_operand={type_operand:?}"
        );
        if physical_arg == Ty::Unit && target.is_reference() {
            let unit = self.cw.fieldref("kotlin/Unit", "INSTANCE", "Lkotlin/Unit;");
            code.getstatic(unit, 1);
        } else if physical_arg.is_jvm_scalar() && target.is_reference() {
            let semantic = self
                .ir
                .logical_types
                .get(&arg)
                .copied()
                .unwrap_or(physical_arg);
            crate::trace_compiler!(
                "value_classes",
                "box coercion arg={arg} physical={physical_arg:?} semantic={semantic:?}"
            );
            box_prim_free(
                self.cw,
                code,
                semantic_scalar_adapter(semantic, physical_arg),
            );
        } else if physical_arg.is_reference() && target.is_jvm_scalar() {
            unbox_prim(self.cw, code, semantic_scalar_adapter(type_operand, target));
        } else if physical_arg.is_jvm_scalar() && target.is_jvm_scalar() && physical_arg != target {
            emit_num_conv(physical_arg, target, code);
        } else {
            implicit_reference_coercion::emit(
                self.ir.expr(arg),
                physical_arg,
                target,
                self.cw,
                code,
            );
        }
    }

    /// Emit a type-operation operand and return its physical stack type plus semantic scalar
    /// identity. Value and branch forms of `is`/`!is` share this boundary so unsigned/value-class
    /// boxing cannot drift between the ordinary and fused emitters.
    pub(super) fn emit_type_op_operand(
        &mut self,
        operand: ExprId,
        code: &mut CodeBuilder,
    ) -> (Ty, Ty) {
        let physical = self
            .ir
            .physical_types
            .get(&operand)
            .map(ir_ty_to_jvm)
            .unwrap_or_else(|| self.value_ty(operand));
        let semantic = self
            .ir
            .logical_types
            .get(&operand)
            .copied()
            .unwrap_or(physical);
        self.emit_value(operand, code);
        (physical, semantic)
    }
}
