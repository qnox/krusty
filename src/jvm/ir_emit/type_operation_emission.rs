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
        // Checked IR facts suppress a guard before its message/method constants are interned.
        // Facts that arise only in emitted control flow remain the responsibility of the finished
        // classfile CFG pass.
        if !physical_arg.is_jvm_scalar() && !self.semantic_non_null(arg) {
            let reread = self.ir.binding_read_stability.get(&arg)
                == Some(&IrBindingStability::Stable)
                && matches!(self.ir.expr(arg), IrExpr::GetValue(_));
            if !reread {
                code.dup();
            }
            code.push_string(
                &format!(
                    "null cannot be cast to non-null type {}",
                    self.rendered_cast_target(target_semantic)
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

    /// A cast target as kotlinc's IR renderer spells it in
    /// `null cannot be cast to non-null type …`.
    fn rendered_cast_target(&self, ty: Ty) -> String {
        let arguments = |arguments: &mut dyn Iterator<Item = Ty>| {
            let rendered: Vec<String> = arguments
                .map(|argument| self.rendered_cast_target(argument))
                .collect();
            if rendered.is_empty() {
                String::new()
            } else {
                format!("<{}>", rendered.join(", "))
            }
        };
        match ty {
            Ty::Unit => "kotlin.Unit".to_string(),
            Ty::Nothing => "kotlin.Nothing".to_string(),
            Ty::Null => "kotlin.Nothing?".to_string(),
            Ty::Error => "<error>".to_string(),
            Ty::Pending => "<pending>".to_string(),
            Ty::Obj(name, types) => format!(
                "{}{}",
                name.render().replace(['/', '$'], "."),
                arguments(&mut types.iter().copied())
            ),
            Ty::Nullable(inner) => format!("{}?", self.rendered_cast_target(*inner)),
            Ty::PlatformNullable(inner) => self.rendered_cast_target(*inner),
            Ty::InProjection(inner) => format!("in {}", self.rendered_cast_target(*inner)),
            Ty::OutProjection(inner) => format!("out {}", self.rendered_cast_target(*inner)),
            Ty::StarProjection(_) => "*".to_string(),
            Ty::TyParam(name, _) => self.rendered_type_parameter(name),
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
        }
    }

    /// Render one declaration-owned type parameter through its recorded semantic identity. The
    /// opaque identity is only compared; its coordinates are never parsed back into an owner.
    fn rendered_type_parameter(&self, identity: &str) -> String {
        let source = crate::types::type_parameter_source_name(identity);
        if let Some((&function, _)) = self
            .ir
            .signatures
            .iter()
            .filter(|(_, signature)| {
                signature
                    .type_params
                    .iter()
                    .any(|parameter| parameter.semantic_name == identity)
            })
            .min_by_key(|(function, _)| *function)
        {
            let declaration = &self.ir.functions[function as usize];
            let owner = declaration.dispatch_receiver.unwrap_or_else(|| {
                self.ir
                    .foreign_template_facade(function)
                    .unwrap_or_else(|| crate::types::type_name(&self.facade))
            });
            let name = self
                .ir
                .vc_declared_sigs
                .get(&function)
                .map_or(declaration.name.as_str(), |(name, _, _)| name.as_str());
            return format!(
                "{source} of {}.{name}",
                owner.render().replace(['/', '$'], ".")
            );
        }
        if let Some((owner, _)) = self.ir.class_signatures().find(|(_, signature)| {
            signature
                .type_params
                .iter()
                .any(|parameter| parameter.semantic_name == identity)
        }) {
            return format!("{source} of {}", owner.render().replace(['/', '$'], "."));
        }
        source.to_string()
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
            // kotlinc materializes the box at its wrapper type and coerces that to the target, so a
            // target between the wrapper and `Object` (`Number`, `Comparable`) is a `checkcast`.
            if !semantic.is_unsigned() {
                if let Some(wrapper) = semantic.non_null().boxed_ref() {
                    self.coerce_reference_on_stack(wrapper, target, code);
                }
            }
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
        // A suspension point the transformer takes leaves its callee's declared result, not the
        // erased result the common IR records.
        let physical = match self.transformed_result(operand) {
            Some(_) => self.value_ty(operand),
            None => self
                .ir
                .physical_types
                .get(&operand)
                .map(ir_ty_to_jvm)
                .unwrap_or_else(|| self.value_ty(operand)),
        };
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
