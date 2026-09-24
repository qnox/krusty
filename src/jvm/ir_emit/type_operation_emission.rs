//! JVM realization of common-IR type operations.
//!
//! Checked common IR fixes the semantic operation and target. This boundary owns wrapper classes,
//! erasure, null-check call shape, reload-versus-dup selection, casts, and numeric representation.

use crate::ir::{ExprId, IrBindingStability, IrExpr, IrTypeOp};
use crate::jvm::classfile::CodeBuilder;
use crate::types::{stored_value_ty, Ty};

use super::{
    box_prim_free, emit_num_conv, implicit_reference_coercion, ir_ty_to_jvm,
    semantic_scalar_adapter, type_descriptor, unbox_prim_from, Emitter,
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
                // kotlinc writes a `checkcast` for every cast and then deletes the ones whose
                // operand already has exactly the target's JVM type (an erasure-narrowing tag
                // where the value is already that type, `List<T>` read tagged `List<Int>`). A cast
                // to `java/lang/Object` from anything narrower stays.
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
                if !redundant {
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
        let physical_internal = if physical_arg.is_jvm_scalar() {
            semantic_scalar_adapter(semantic_arg, physical_arg)
                .boxed_ref()
                .map(crate::jvm::names::instanceof_internal_name)
        } else {
            Some(crate::jvm::names::instanceof_internal_name(physical_arg))
        };
        // kotlinc guards the operand unless its nullability analysis proves it non-null; a boxed
        // scalar is a fresh wrapper.
        if !physical_arg.is_jvm_scalar() && !self.known_non_null(arg) {
            let reread = self.ir.binding_read_stability.get(&arg)
                == Some(&IrBindingStability::Stable)
                && matches!(self.ir.expr(arg), IrExpr::GetValue(_));
            if !reread {
                code.dup();
            }
            code.push_string(
                &format!(
                    "null cannot be cast to non-null type {}",
                    rendered_cast_target(target_semantic)
                ),
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
        }
        // kotlinc writes a `checkcast` for every cast and then deletes the ones whose operand
        // already has exactly the target's JVM type; a cast to `java/lang/Object` from anything
        // narrower stays.
        if physical_internal.as_deref() != Some(internal) {
            let class = self.cw.class_ref(internal);
            code.checkcast(class);
        }
        // A successful non-null cast to a Kotlin scalar produces its value representation.
        if jvm_ty.is_jvm_scalar() {
            unbox_prim_from(
                self.cw,
                code,
                Ty::obj(internal),
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
            unbox_prim_from(
                self.cw,
                code,
                physical_arg,
                semantic_scalar_adapter(type_operand, target),
            );
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

/// A cast target as kotlinc's IR renderer spells it in `null cannot be cast to non-null type …`:
/// the classifier's fully qualified name with its type arguments, projections and nullability, and
/// a function type as the `FunctionN` (or `SuspendFunctionN`) interface it stands for.
fn rendered_cast_target(ty: Ty) -> String {
    let arguments = |arguments: &mut dyn Iterator<Item = Ty>| {
        let rendered: Vec<String> = arguments.map(rendered_cast_target).collect();
        if rendered.is_empty() {
            String::new()
        } else {
            format!("<{}>", rendered.join(", "))
        }
    };
    match ty {
        Ty::Unit => "kotlin.Unit".to_string(),
        Ty::Nothing => "kotlin.Nothing".to_string(),
        Ty::Obj(name, types) => format!(
            "{}{}",
            name.render().replace(['/', '$'], "."),
            arguments(&mut types.iter().copied())
        ),
        Ty::Nullable(inner) => format!("{}?", rendered_cast_target(*inner)),
        Ty::PlatformNullable(inner) => rendered_cast_target(*inner),
        Ty::InProjection(inner) => format!("in {}", rendered_cast_target(*inner)),
        Ty::OutProjection(inner) => format!("out {}", rendered_cast_target(*inner)),
        Ty::StarProjection(_) => "*".to_string(),
        Ty::TyParam(name, _) => crate::types::type_parameter_source_name(name).to_string(),
        Ty::Fun(signature) => format!(
            "{}{}{}",
            if signature.suspend {
                "kotlin.coroutines.SuspendFunction"
            } else {
                "kotlin.Function"
            },
            signature.params.len(),
            arguments(&mut signature.params.iter().copied().chain([signature.ret]))
        ),
        _ => "kotlin.Any".to_string(),
    }
}
