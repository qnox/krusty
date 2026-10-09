//! Kotlin class literals: the `java.lang.Class` token kotlinc's `generateClassLiteralReference`
//! loads, wrapped into a `KClass`.

use super::Ty;
use crate::ir::ExprId;
use crate::jvm::classfile::CodeBuilder;

impl super::Emitter<'_> {
    /// `X::class` or `value::class`. A primitive class names the primitive's own class
    /// (`Integer.TYPE`), as `generateClassInstance` does without `wrapPrimitives`; a type parameter,
    /// even one an inlined call substituted by a primitive, and any other classifier load its
    /// boxed class constant. A bound literal asks its value's runtime class.
    pub(super) fn emit_kclass_literal(
        &mut self,
        classifier: Option<Ty>,
        value: Option<ExprId>,
        type_argument: bool,
        code: &mut CodeBuilder,
    ) {
        match (classifier, value) {
            (Some(classifier), None) => match primitive_wrapper(classifier) {
                Some(wrapper) if !type_argument => self.primitive_class(wrapper, code),
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
            (None, Some(value)) => {
                self.emit_value(value, code);
                self.box_scalar_operand(self.value_ty(value), code);
                let get_class =
                    self.cw
                        .methodref("java/lang/Object", "getClass", "()Ljava/lang/Class;");
                code.invokevirtual(get_class, 0, 1);
            }
            _ => {
                self.run.set_emit_error(
                    "checked class literal has an invalid operand shape".to_string(),
                );
                return;
            }
        }
        let reflection = self.cw.methodref(
            "kotlin/jvm/internal/Reflection",
            "getOrCreateKotlinClass",
            "(Ljava/lang/Class;)Lkotlin/reflect/KClass;",
        );
        code.invokestatic(reflection, 1, 1);
    }

    /// A primitive's class, which its wrapper holds in the static `TYPE`.
    fn primitive_class(&mut self, wrapper: &str, code: &mut CodeBuilder) {
        let field = self.cw.fieldref(wrapper, "TYPE", "Ljava/lang/Class;");
        code.getstatic(field, 1);
    }
}

/// The wrapper class of a JVM primitive type; `None` for a reference or an unsigned value class.
fn primitive_wrapper(ty: Ty) -> Option<&'static str> {
    (ty.scalar_value_repr() == Some(ty))
        .then(|| crate::jvm::jvm_class_map::wrapper_internal(ty))
        .flatten()
}
