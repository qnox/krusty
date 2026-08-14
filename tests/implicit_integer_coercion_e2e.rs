use super::common;

const ANNOTATION: &str = "package kotlin.internal\n\
annotation class ImplicitIntegerCoercion\n";

#[test]
fn annotated_constants_coerce_to_annotated_unsigned_parameters() {
    const USE_SITE: &str = "// LANGUAGE: +ImplicitSignedToUnsignedIntegerConversion\n\
package sample\n\
import kotlin.internal.ImplicitIntegerCoercion\n\
@ImplicitIntegerCoercion const val BYTE_VALUE = 255\n\
@ImplicitIntegerCoercion const val LONG_VALUE = 255L\n\
@ImplicitIntegerCoercion const val OVERFLOWING_BYTE = 256\n\
fun bytes(@ImplicitIntegerCoercion vararg values: UByte) {}\n\
fun accept(@ImplicitIntegerCoercion value: UShort) {}\n\
fun test() {\n\
    bytes(BYTE_VALUE, LONG_VALUE)\n\
    accept(OVERFLOWING_BYTE)\n\
}\n";
    common::expect_front_end_ok_files_with_stdlib(
        &[ANNOTATION, USE_SITE],
        "annotated integer constants",
    );
}

#[test]
fn implicit_integer_coercion_requires_the_language_feature() {
    const USE_SITE: &str = "package sample\n\
import kotlin.internal.ImplicitIntegerCoercion\n\
@ImplicitIntegerCoercion const val VALUE = 1\n\
fun accept(@ImplicitIntegerCoercion value: UInt) {}\n\
fun test() { accept(VALUE) }\n";
    let diagnostics = common::front_end_diagnostics_files(
        &[ANNOTATION, USE_SITE],
        std::slice::from_ref(&common::stdlib_jar()),
        Some(common::jdk_modules().as_path()),
    );
    assert!(
        diagnostics
            .iter()
            .any(|diagnostic| diagnostic.contains("argument type mismatch")),
        "feature-disabled call was accepted: {diagnostics:?}"
    );
}

#[test]
fn implicit_integer_coercion_uses_qualified_annotation_identity() {
    const IMPOSTOR: &str = "package fake\n\
annotation class ImplicitIntegerCoercion\n";
    const USE_SITE: &str = "// LANGUAGE: +ImplicitSignedToUnsignedIntegerConversion\n\
package sample\n\
import fake.ImplicitIntegerCoercion\n\
@ImplicitIntegerCoercion const val VALUE = 1\n\
fun accept(@ImplicitIntegerCoercion value: UInt) {}\n\
fun test() { accept(VALUE) }\n";
    let diagnostics = common::front_end_diagnostics_files(
        &[IMPOSTOR, USE_SITE],
        std::slice::from_ref(&common::stdlib_jar()),
        Some(common::jdk_modules().as_path()),
    );
    assert!(
        diagnostics
            .iter()
            .any(|diagnostic| diagnostic.contains("argument type mismatch")),
        "wrong annotation identity enabled coercion: {diagnostics:?}"
    );
}
