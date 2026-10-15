//! Kotlin class literals: the `java.lang.Class` token kotlinc's `generateClassLiteralReference`
//! loads, wrapped into a `KClass`.

use super::Ty;
use crate::ir::{ExprId, IrExpr};
use crate::jvm::classfile::CodeBuilder;
use crate::types::TypeName;

impl super::Emitter<'_> {
    /// `X::class` or `value::class`: the class token wrapped into a `KClass`.
    pub(super) fn emit_kclass_literal(
        &mut self,
        classifier: Option<Ty>,
        value: Option<ExprId>,
        type_argument: bool,
        code: &mut CodeBuilder,
    ) {
        if !self.emit_java_class_token(classifier, value, type_argument, false, code) {
            return;
        }
        let reflection = self.cw.methodref(
            "kotlin/jvm/internal/Reflection",
            "getOrCreateKotlinClass",
            "(Ljava/lang/Class;)Lkotlin/reflect/KClass;",
        );
        code.invokestatic(reflection, 1, 1);
    }

    /// Whether `operand` is a Kotlin class literal, including a reified `T::class` placeholder in an
    /// emitted inline body: kotlinc's `isClassValue` operands of a class-token comparison.
    pub(super) fn is_class_value(&self, operand: ExprId) -> bool {
        matches!(
            self.ir.expr(operand),
            IrExpr::KClassLiteral { .. } | IrExpr::ReifiedClassMarker { kclass: true, .. }
        )
    }

    /// The `java.lang.Class` token of a class value operand (see [`Self::is_class_value`]), with
    /// a primitive loading its wrapper class as kotlinc's `Equals` does for `if_acmp*`.
    pub(super) fn emit_class_value_token(&mut self, operand: ExprId, code: &mut CodeBuilder) {
        match self.ir.expr(operand).clone() {
            IrExpr::KClassLiteral {
                classifier,
                value,
                type_argument,
            } => {
                self.emit_java_class_token(classifier, value, type_argument, true, code);
            }
            IrExpr::ReifiedClassMarker { name, erased, .. } => {
                self.emit_reified_class_token(&name, erased, code);
            }
            _ => self
                .run
                .set_emit_error("class-token comparison over a non-class operand".to_string()),
        }
    }

    /// kotlinc's reified class placeholder: `reifiedOperationMarker(4, "T")` then the erased
    /// class constant, a pair a splicer patches with the call-site class.
    pub(super) fn emit_reified_class_token(
        &mut self,
        name: &str,
        erased: TypeName,
        code: &mut CodeBuilder,
    ) {
        code.push_int(4, self.cw);
        code.push_string(name, self.cw);
        let marker = self.cw.methodref(
            "kotlin/jvm/internal/Intrinsics",
            "reifiedOperationMarker",
            "(ILjava/lang/String;)V",
        );
        code.invokestatic(marker, 2, 0);
        code.ldc_class(&erased.render(), self.cw);
    }

    /// The `java.lang.Class` token of a class literal, kotlinc's `generateClassLiteralReference`
    /// before `wrapIntoKClass`. A primitive names its own class (`Integer.TYPE`) unless
    /// `wrap_primitives` asks for the wrapper's; a type parameter, even one an inlined call
    /// substituted by a primitive, and any other classifier load their boxed class constant. A bound
    /// literal asks its value's runtime class, except that a primitive value is discarded and its
    /// static class loaded instead (`JavaClassProperty.invokeWith`). Returns `false` after
    /// recording an emit error for a malformed literal.
    fn emit_java_class_token(
        &mut self,
        classifier: Option<Ty>,
        value: Option<ExprId>,
        type_argument: bool,
        wrap_primitives: bool,
        code: &mut CodeBuilder,
    ) -> bool {
        match (classifier, value) {
            (Some(classifier), None) => match primitive_wrapper(classifier) {
                Some(wrapper) if !type_argument => {
                    self.primitive_class(wrapper, wrap_primitives, code)
                }
                _ => {
                    // `Unit::class` is the `kotlin.Unit` object's class, never the `void` return.
                    let classifier = crate::types::stored_value_ty(classifier);
                    let descriptor = super::boxed_descriptor(classifier);
                    let internal = descriptor
                        .strip_prefix('L')
                        .and_then(|descriptor| descriptor.strip_suffix(';'))
                        .unwrap_or(&descriptor);
                    code.ldc_class(internal, self.cw);
                }
            },
            (None, Some(value)) => match primitive_wrapper(self.value_ty(value)) {
                Some(wrapper) => {
                    self.emit_discarding(value, code);
                    self.primitive_class(wrapper, wrap_primitives, code);
                }
                None => {
                    self.emit_value(value, code);
                    self.box_scalar_operand(self.value_ty(value), code);
                    let get_class =
                        self.cw
                            .methodref("java/lang/Object", "getClass", "()Ljava/lang/Class;");
                    code.invokevirtual(get_class, 0, 1);
                }
            },
            _ => {
                self.run.set_emit_error(
                    "checked class literal has an invalid operand shape".to_string(),
                );
                return false;
            }
        }
        true
    }

    /// A primitive's class, which its wrapper holds in the static `TYPE`, or with
    /// `wrap_primitives` the wrapper class itself.
    fn primitive_class(&mut self, wrapper: &str, wrap_primitives: bool, code: &mut CodeBuilder) {
        if wrap_primitives {
            code.ldc_class(wrapper, self.cw);
        } else {
            let field = self.cw.fieldref(wrapper, "TYPE", "Ljava/lang/Class;");
            code.getstatic(field, 1);
        }
    }
}

/// The wrapper class of a JVM primitive type; `None` for a reference or an unsigned value class.
fn primitive_wrapper(ty: Ty) -> Option<&'static str> {
    (ty.scalar_value_repr() == Some(ty))
        .then(|| crate::jvm::jvm_class_map::wrapper_internal(ty))
        .flatten()
}
