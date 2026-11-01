//! JVM method attributes derived from a function's recorded declaration annotations.

use super::*;

/// Add one abstract declaration with the member's own visibility and apply its generic signature
/// and source annotations.
pub(super) fn add_abstract(
    ir: &IrFile,
    cw: &mut ClassWriter,
    function: u32,
    formatter: &JvmSignatureFormatter<'_>,
    env: &super::EmitEnv<'_>,
) -> String {
    let override_results = env.override_results;
    let declaration = &ir.functions[function as usize];
    let descriptor = declared_method_desc(ir, override_results, function);
    cw.add_abstract_method_sig(
        method_access::abstract_method_access(ir, function),
        &declaration.name,
        &descriptor,
        declared_method_signature(formatter, ir, override_results, function).as_deref(),
    );
    // An abstract member declares its parameters as a concrete one does, `$this$name` included.
    if env.java_parameters {
        let parameters = crate::jvm::method_parameters::function(
            ir,
            function,
            &super::jvm_function_params(ir, function),
            None,
        );
        cw.set_method_parameters(&declaration.name, &descriptor, &parameters);
    }
    emit_recorded(ir, cw, function, &declaration.name, &descriptor);
    // An abstract method still carries kotlinc's nullability annotations, in an interface, an
    // abstract class and an enum alike, classified from its declaration as a concrete method's
    // are: a value-class parameter is `@NotNull` whatever its carrier admits.
    let super::declared_nullability::DeclaredNullability { result, parameters } =
        super::override_result_emission::declared_nullability(ir, override_results, function);
    if result.is_some() || parameters.iter().any(Option::is_some) {
        cw.set_method_nullability(&declaration.name, &descriptor, result, &parameters);
    }
    descriptor
}

/// Write a declaration's recorded annotations onto a method the caller has already added.
///
/// Concrete and abstract members share this boundary. `@Deprecated` also sets the JVM attribute,
/// and `DeprecationLevel.HIDDEN` marks the method synthetic, matching kotlinc's class file.
pub(super) fn emit_recorded(
    ir: &IrFile,
    cw: &mut ClassWriter,
    function: u32,
    name: &str,
    descriptor: &str,
) {
    if let Some(annotations) = ir.function_annotations.get(&function) {
        emit_declared(cw, annotations, name, descriptor);
    }
}

/// Write a method's declared annotations, with the attributes `@Deprecated` implies.
pub(super) fn emit_declared(
    cw: &mut ClassWriter,
    annotations: &crate::ir::DeclarationAnnotations,
    name: &str,
    descriptor: &str,
) {
    cw.set_method_annotations(name, descriptor, annotations);
    if annotations.deprecated() {
        cw.mark_method_deprecated(name, descriptor);
    }
    if hidden_realization_is_synthetic(annotations) {
        cw.set_method_synthetic(name, descriptor);
    }
}

/// kotlinc emits a `@Deprecated(level = HIDDEN)` declaration `ACC_SYNTHETIC`, so Java cannot call
/// it either, unless it also carries `@java.lang.Deprecated`: that annotation keeps the method
/// visible to Java (KT-80649), which kotlinc's no-arg plugin relies on for its constructor.
pub(super) fn hidden_realization_is_synthetic(
    annotations: &crate::ir::DeclarationAnnotations,
) -> bool {
    annotations.deprecated_hidden()
        && !annotations
            .applications()
            .any(|annotation| annotation.internal.matches("java/lang/Deprecated"))
}

/// Whether kotlinc annotates the nullability of `function`'s declared method.
///
/// Only declared methods a source caller can reach carry `@NotNull`/`@Nullable`. A synthetic one
/// (HIDDEN-deprecated, or reifiable: only an inlining call site runs it), a PRIVATE one (including
/// a data class's `copy` under `DataClassCopyRespectsConstructorVisibility`) and a
/// compiler-invented accessor (`access$…$cp`, which gets no `Signature` either) carry neither: the
/// annotations exist for Java interop, which cannot see them.
pub(super) fn nullability_annotated(
    ir: &IrFile,
    function: u32,
    declared: &crate::ir::DeclarationAnnotations,
) -> bool {
    !declared.deprecated_hidden()
        && !method_access::is_reifiable(ir, function)
        && !ir.method_visibility(function).is_private()
        && !ir.synthetic_methods.contains(&function)
        && !ir.jvm_nullability_unannotated_methods.contains(&function)
}
