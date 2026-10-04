//! JVM method attributes derived from a function's recorded declaration annotations.

use super::*;

/// Add one public abstract declaration and apply its generic signature and source annotations.
pub(super) fn add_abstract(
    ir: &IrFile,
    cw: &mut ClassWriter,
    function: u32,
    formatter: &JvmSignatureFormatter<'_>,
    override_results: &crate::jvm::override_results::OverrideResults,
) -> String {
    let declaration = &ir.functions[function as usize];
    let descriptor = declared_method_desc(ir, override_results, function);
    cw.add_abstract_method_sig(
        0x0001 | 0x0400,
        &declaration.name,
        &descriptor,
        declared_method_signature(formatter, ir, override_results, function).as_deref(),
    );
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
    if annotations.deprecated_hidden() {
        cw.set_method_synthetic(name, descriptor);
    }
}
