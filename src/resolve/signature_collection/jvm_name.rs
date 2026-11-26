//! The JVM name a declaration's `@JvmName` annotation selects, resolved through the file's class
//! names and carried as the declaration's own name from signature collection on.

use super::*;

pub(super) fn resolved_jvm_name(
    file: &File,
    function: &FunDecl,
    class_names: &ClassNames,
) -> String {
    let annotation = function
        .annotations
        .iter()
        .enumerate()
        .find(|(_, annotation)| {
            class_names
                .get_class(&annotation.name)
                .is_some_and(|name| name.matches("kotlin/jvm/JvmName"))
        });
    annotation
        .and_then(|(index, _)| {
            function
                .annotation_args
                .get(index)
                .and_then(|arguments| arguments.first())
                .and_then(|&argument| match file.expr(argument) {
                    Expr::StringLit(value) => value.as_str().map(str::to_string),
                    _ => None,
                })
        })
        .unwrap_or_else(|| function.name.clone())
}

pub(super) fn resolved_compact_jvm_name(
    headers: &crate::fir::StreamedHeaderModule,
    declaration: crate::fir::DeclarationId,
    annotations: &[TypeRef],
    class_names: &ClassNames,
    fallback: &str,
) -> String {
    annotations
        .iter()
        .enumerate()
        .find(|(_, annotation)| {
            class_names
                .get_class(&annotation.name)
                .is_some_and(|name| name.matches("kotlin/jvm/JvmName"))
        })
        .and_then(|(index, _)| {
            headers
                .annotation_string_arguments(declaration, index)
                .first()
                .map(|value| value.to_string())
        })
        .unwrap_or_else(|| fallback.to_string())
}
