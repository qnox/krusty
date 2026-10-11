use super::common;

const ANNOTATION: &str = "package kotlin.internal\n\
annotation class ImplicitIntegerCoercion\n";

#[test]
fn annotated_constants_coerce_to_annotated_unsigned_parameters() {
    const USE_SITE: &str = "// LANGUAGE: +ImplicitSignedToUnsignedIntegerConversion\n\
@file:OptIn(ExperimentalUnsignedTypes::class)\n\
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
fn cross_file_constant_selection_keeps_the_coercion_fact() {
    const DECLARATION: &str = "package values\n\
import kotlin.internal.ImplicitIntegerCoercion\n\
@ImplicitIntegerCoercion const val VALUE = 255\n";
    const USE_SITE: &str = "// LANGUAGE: +ImplicitSignedToUnsignedIntegerConversion\n\
package sample\n\
import kotlin.internal.ImplicitIntegerCoercion\n\
import values.VALUE\n\
fun accept(@ImplicitIntegerCoercion value: UByte) {}\n\
fun test() { accept(VALUE) }\n";

    common::expect_front_end_ok_files_with_stdlib(
        &[ANNOTATION, DECLARATION, USE_SITE],
        "cross-file annotated integer constant",
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

#[test]
fn signed_values_widen_to_the_unsigned_parameter_carrier() {
    const USE_SITE: &str = "// LANGUAGE: +ImplicitSignedToUnsignedIntegerConversion\n\
import kotlin.internal.ImplicitIntegerCoercion\n\
@ImplicitIntegerCoercion const val IMPLICIT_INT = 255\n\
@ImplicitIntegerCoercion const val EXPLICIT_INT: Int = 255\n\
@ImplicitIntegerCoercion const val BIGGER_THAN_UBYTE = 256\n\
fun testInt(@ImplicitIntegerCoercion x: UInt) = x\n\
fun testLong(@ImplicitIntegerCoercion x: ULong) = x\n\
fun takeUByte(@ImplicitIntegerCoercion u: UByte) = u\n\
fun takeUShort(@ImplicitIntegerCoercion u: UShort) = u\n\
fun takeUInt(@ImplicitIntegerCoercion u: UInt) = u\n\
fun takeULong(@ImplicitIntegerCoercion u: ULong) = u\n\
fun takeUBytes(@ImplicitIntegerCoercion vararg u: UByte) = u[0].toInt() + u[1].toInt() + u[2].toInt()\n\
fun box(): String {\n\
    if (testInt(5) != 5u) return \"int\"\n\
    if (testInt(x = 5) != 5u) return \"named int\"\n\
    if (testLong(5) != 5uL) return \"long\"\n\
    if (testLong(x = 5) != 5uL) return \"named long\"\n\
    if (takeUByte(255) != 255.toUByte()) return \"literal ubyte\"\n\
    if (takeUByte(IMPLICIT_INT) != 255.toUByte()) return \"ubyte\"\n\
    if (takeUByte(EXPLICIT_INT) != 255.toUByte()) return \"explicit\"\n\
    if (takeUShort(IMPLICIT_INT) != 255.toUShort()) return \"ushort\"\n\
    if (takeUShort(BIGGER_THAN_UBYTE) != 256.toUShort()) return \"256\"\n\
    if (takeUInt(IMPLICIT_INT) != 255u) return \"uint\"\n\
    if (takeULong(IMPLICIT_INT) != 255uL) return \"ulong\"\n\
    if (takeUBytes(IMPLICIT_INT, EXPLICIT_INT, 42u) != 255 + 255 + 42) return \"vararg\"\n\
    return \"OK\"\n\
}\n";
    let sources = [("annotation.kt", ANNOTATION), ("Main.kt", USE_SITE)];
    let krusty = common::compile_and_run_files_with_stdlib(&sources)
        .expect("signed-to-unsigned coercion did not run box()");
    let work = common::scratch_dir().expect("cannot allocate coercion fixture");
    let source_paths = sources
        .iter()
        .map(|(name, source)| {
            let path = work.join(name);
            std::fs::write(&path, source).expect("write coercion fixture");
            path
        })
        .collect::<Vec<_>>();
    let output = work.join("out");
    std::fs::create_dir_all(&output).expect("create kotlinc output");
    // The test-only feature is a JVM system property. It stays in the argument list so the
    // invocation fingerprint records it. A cache hit replays the class files; a miss starts a
    // server keyed by the property and does not touch the default compiler pool.
    let mut args = vec![
        "-Dkotlinc.test.allow.testonly.language.features=true".to_string(),
        "-Xallow-kotlin-package".to_string(),
        "-XXLanguage:+ImplicitSignedToUnsignedIntegerConversion".to_string(),
        "-d".to_string(),
        output.to_string_lossy().into_owned(),
    ];
    for path in &source_paths {
        args.push(path.to_string_lossy().into_owned());
    }
    let (code, stderr) = common::kotlinc_compile(&args).expect("reference compiler unavailable");
    assert!(
        code == 0,
        "kotlinc rejected signed-to-unsigned coercion: {stderr}"
    );
    let reference = common::run_box(
        &[],
        "MainKt",
        &[output, common::stdlib_jar(), common::jdk_modules()],
    )
    .expect("run kotlinc signed-to-unsigned fixture");
    let _ = std::fs::remove_dir_all(work);
    assert_eq!(reference, "OK", "kotlinc fixture must succeed");
    assert_eq!(krusty, reference, "krusty and kotlinc box results differ");
}
