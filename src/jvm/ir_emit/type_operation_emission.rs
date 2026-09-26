//! JVM realization of common-IR type operations.
//!
//! Checked common IR fixes the semantic operation and target. This boundary owns wrapper classes,
//! erasure, null-check call shape, reload-versus-dup selection, casts, and numeric representation.

use crate::ir::TypeCheckRole;
use crate::ir::{ExprId, IrBindingStability, IrExpr, IrTypeOp};
use crate::jvm::classfile::CodeBuilder;
use crate::jvm::type_intrinsics::{cast, instance_check, IntrinsicCall};
use crate::types::{stored_value_ty, Ty};

use super::{
    box_prim_free, emit_num_conv, implicit_reference_coercion, ir_ty_to_jvm,
    semantic_scalar_adapter, type_descriptor, unbox_prim_from, Emitter,
};

impl Emitter<'_> {
    pub(super) fn emit_type_operation(
        &mut self,
        expression: ExprId,
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
        let arg = match op {
            IrTypeOp::ImplicitCoercion => self.unboxed_reference_source(arg, type_operand),
            _ => arg,
        };
        let (physical_arg, semantic_arg) = self.emit_type_op_operand(arg, code);
        match op {
            IrTypeOp::InstanceOf | IrTypeOp::NotInstanceOf if type_operand.is_nullable() => {
                if physical_arg.is_jvm_scalar() {
                    box_prim_free(
                        self.cw,
                        code,
                        semantic_scalar_adapter(semantic_arg, physical_arg),
                    );
                }
                self.emit_nullable_instance_check(&internal, type_operand, code);
                if op == IrTypeOp::NotInstanceOf {
                    self.emit_negated_instance_result(code);
                }
            }
            IrTypeOp::InstanceOf => {
                if physical_arg.is_jvm_scalar() {
                    box_prim_free(
                        self.cw,
                        code,
                        semantic_scalar_adapter(semantic_arg, physical_arg),
                    );
                }
                self.emit_instance_check(&internal, type_operand, code);
            }
            IrTypeOp::NotInstanceOf => {
                if physical_arg.is_jvm_scalar() {
                    box_prim_free(
                        self.cw,
                        code,
                        semantic_scalar_adapter(semantic_arg, physical_arg),
                    );
                }
                self.emit_instance_check(&internal, type_operand, code);
                self.emit_negated_instance_result(code);
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
                // Only a written `as` asks `TypeIntrinsics`; a compiler-inserted narrowing (an
                // `as?` after its `is`) is kotlinc's implicit cast, a plain `checkcast`.
                if let Some(intrinsic) = self.written_cast_intrinsic(expression, type_operand) {
                    self.emit_intrinsic_cast(&internal, intrinsic, code);
                    return;
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
                    expression,
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

    /// `is T?` for a type the checker could not expand into `x == null || x is T`: a reified type
    /// argument substituted with a nullable type. kotlinc's `generateIsCheck` accepts `null`
    /// before the `instanceof` itself.
    pub(super) fn emit_nullable_instance_check(
        &mut self,
        internal: &str,
        type_operand: Ty,
        code: &mut CodeBuilder,
    ) {
        let null = code.new_label();
        let end = code.new_label();
        code.dup();
        code.ifnull(null);
        self.emit_instance_check(internal, type_operand, code);
        code.goto(end);
        code.bind(null);
        code.pop();
        code.push_int(1, self.cw);
        code.bind(end);
    }

    /// kotlinc's `TypeIntrinsics.instanceOf`: an `instanceof`, or the `TypeIntrinsics` check that
    /// replaces it for a mutable collection or a function type. Leaves an `int` 0/1.
    pub(super) fn emit_instance_check(
        &mut self,
        internal: &str,
        type_operand: Ty,
        code: &mut CodeBuilder,
    ) {
        match self.ir.type_check_role(type_operand) {
            Some(intrinsic) => self.emit_type_intrinsic_call(&instance_check(intrinsic), code),
            None => {
                let class = self.cw.class_ref(internal);
                code.instance_of(class);
            }
        }
    }

    /// A value `!is`: kotlinc negates the 0/1 instance result with a branch, not an `ixor`.
    pub(super) fn emit_negated_instance_result(&mut self, code: &mut CodeBuilder) {
        let instance = code.new_label();
        let end = code.new_label();
        code.ifne(instance);
        code.push_int(1, self.cw);
        code.goto(end);
        code.bind(instance);
        code.push_int(0, self.cw);
        code.bind(end);
    }

    /// The `TypeIntrinsics` cast a written `as` of `expression` to `type_operand` needs, if any.
    fn written_cast_intrinsic(
        &self,
        expression: ExprId,
        type_operand: Ty,
    ) -> Option<TypeCheckRole> {
        self.ir
            .type_check_role(type_operand)
            .filter(|_| self.ir.written_casts.contains(&expression))
    }

    /// kotlinc's `TypeIntrinsics.checkcast` for a non-safe cast to a mutable collection or a
    /// function type.
    fn emit_intrinsic_cast(
        &mut self,
        internal: &str,
        intrinsic: TypeCheckRole,
        code: &mut CodeBuilder,
    ) {
        let (call, checkcast) = cast(intrinsic);
        self.emit_type_intrinsic_call(&call, code);
        if checkcast {
            let class = self.cw.class_ref(internal);
            code.checkcast(class);
        }
    }

    fn emit_type_intrinsic_call(&mut self, call: &IntrinsicCall, code: &mut CodeBuilder) {
        if let Some(arity) = call.arity {
            code.push_int(i32::from(arity), self.cw);
        }
        let method = self
            .cw
            .methodref(call.owner(), &call.name, &call.descriptor);
        code.invokestatic(method, if call.arity.is_some() { 2 } else { 1 }, 1);
    }

    fn emit_non_null_cast(
        &mut self,
        expression: ExprId,
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
        if let Some(intrinsic) = self.written_cast_intrinsic(expression, type_operand) {
            self.emit_intrinsic_cast(internal, intrinsic, code);
        } else if physical_internal.as_deref() != Some(internal) {
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
        self.rendered_cast_type(ty, RootPackage::Unqualified)
    }

    /// A reified cast target as kotlinc's inliner spells it in the same message, where a class in
    /// the root package reads `<root>.Token`.
    pub(super) fn rendered_inlined_cast_target(&self, ty: Ty) -> String {
        self.rendered_cast_type(ty, RootPackage::Qualified)
    }

    fn rendered_cast_type(&self, ty: Ty, root: RootPackage) -> String {
        let arguments = |arguments: &mut dyn Iterator<Item = Ty>| {
            let rendered: Vec<String> = arguments
                .map(|argument| self.rendered_cast_type(argument, root))
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
                "{}{}{}",
                if root == RootPackage::Qualified && name.package_matches("") {
                    "<root>."
                } else {
                    ""
                },
                name.render().replace(['/', '$'], "."),
                arguments(&mut types.iter().copied())
            ),
            Ty::Nullable(inner) => format!("{}?", self.rendered_cast_type(*inner, root)),
            Ty::PlatformNullable(inner) => self.rendered_cast_type(*inner, root),
            Ty::InProjection(inner) => format!("in {}", self.rendered_cast_type(*inner, root)),
            Ty::OutProjection(inner) => format!("out {}", self.rendered_cast_type(*inner, root)),
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

    /// The reference an unboxing coercion reads. kotlinc coerces a value from the type it was
    /// produced at straight to the primitive, so an implicit reference coercion directly beneath
    /// the unbox (`ArrayList<Int>.get` producing `Object`, coerced to `Int!` and then to `int`)
    /// writes no `checkcast` of its own: the unbox reads `Object` and goes through `Number`.
    fn unboxed_reference_source(&self, arg: ExprId, type_operand: Ty) -> ExprId {
        if !ir_ty_to_jvm(&stored_value_ty(type_operand)).is_jvm_scalar() {
            return arg;
        }
        let IrExpr::TypeOp {
            op: IrTypeOp::ImplicitCoercion,
            arg: source,
            type_operand: intermediate,
        } = self.ir.expr(arg)
        else {
            return arg;
        };
        let source_is_reference = self
            .ir
            .physical_types
            .get(source)
            .map(ir_ty_to_jvm)
            .unwrap_or_else(|| self.value_ty(*source))
            .is_reference();
        if source_is_reference && ir_ty_to_jvm(&stored_value_ty(*intermediate)).is_reference() {
            *source
        } else {
            arg
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

/// Whether a cast message qualifies a root-package class with `<root>.`, as kotlinc's inliner does.
#[derive(Clone, Copy, PartialEq, Eq)]
enum RootPackage {
    Unqualified,
    Qualified,
}
