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
    let Some(annotations) = ir.function_annotations.get(&function) else {
        return;
    };
    cw.set_method_annotations(name, descriptor, annotations);
    if annotations.deprecated() {
        cw.mark_method_deprecated(name, descriptor);
    }
    if annotations.deprecated_hidden() {
        cw.set_method_synthetic(name, descriptor);
    }
}
