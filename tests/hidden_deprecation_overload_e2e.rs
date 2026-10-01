//! Hidden deprecation is a provider-normalized declaration fact. It removes a declaration from
//! Kotlin overload identity and candidate collection without removing its backend realization.

use super::common;

fn diagnostics(source: &str) -> common::CompilerDiagnosticResult {
    common::compiler_diagnostics(&[("HiddenOverload.kt", source)], &[common::stdlib_jar()])
}

#[test]
fn hidden_member_with_a_different_return_is_not_a_source_overload() {
    let source = r#"
interface Printer {
    fun print(vararg values: Any?): Printer
}
class SmartPrinter : Printer {
    override fun print(vararg values: Any?): SmartPrinter = this

    @Deprecated("binary compatibility", level = DeprecationLevel.HIDDEN)
    fun print(values: Array<Any?>) {
        print(*values)
    }
}
fun box(): String {
    val printer = SmartPrinter()
    return if (printer.print("x") === printer) "OK" else "FAIL"
}
"#;

    let result = diagnostics(source);
    assert_eq!(result.reference_code, 0, "{}", result.reference_stderr);
    assert_eq!(
        result.krusty_code, 0,
        "{}{}",
        result.krusty_stdout, result.krusty_stderr
    );
    common::expect_box_ok_with_stdlib(source, "hidden member with different return");
}

#[test]
fn imported_hidden_entry_has_the_same_semantic_identity() {
    let source = r#"
import kotlin.DeprecationLevel.HIDDEN

class C {
    fun choose(vararg values: Any?): C = this

    @Deprecated("binary compatibility", level = HIDDEN)
    fun choose(values: Array<Any?>) {}
}
fun box(): String {
    val subject = C()
    return if (subject.choose("x") === subject) "OK" else "FAIL"
}
"#;

    let result = diagnostics(source);
    assert_eq!(result.reference_code, 0, "{}", result.reference_stderr);
    assert_eq!(
        result.krusty_code, 0,
        "{}{}",
        result.krusty_stdout, result.krusty_stderr
    );
    common::expect_box_ok_with_stdlib(source, "imported hidden entry");
}

#[test]
fn hidden_array_overload_is_not_selected() {
    let source = r#"
class C {
    fun choose(vararg values: Any?): Int = values.size

    @Deprecated("binary compatibility", level = DeprecationLevel.HIDDEN)
    fun choose(values: Array<Any?>): Long = values.size.toLong()
}
fun box(): String = if (C().choose(arrayOf<Any?>("x", "y")) == 1) "OK" else "FAIL"
"#;

    let result = diagnostics(source);
    assert_eq!(result.reference_code, 0, "{}", result.reference_stderr);
    assert_eq!(
        result.krusty_code, 0,
        "{}{}",
        result.krusty_stdout, result.krusty_stderr
    );
    common::expect_box_ok_with_stdlib(source, "hidden overload selection");
}

#[test]
fn identical_hidden_and_visible_realizations_are_a_platform_clash() {
    let source = r#"
class C {
    @Deprecated("binary compatibility", level = DeprecationLevel.HIDDEN)
    fun f(value: Int): Int = value

    fun f(value: Int): Int = value + 1
}
"#;

    let result = diagnostics(source);
    common::expect_identical_rejection(&result, "hidden and visible JVM realizations");
}

#[test]
fn warning_deprecation_remains_in_source_overload_identity() {
    let source = r#"
class C {
    @Deprecated("still source-visible", level = DeprecationLevel.WARNING)
    fun f(value: Int): Long = value.toLong()

    fun f(value: Int): Int = value
}
"#;

    let result = diagnostics(source);
    common::expect_identical_rejection(&result, "warning-deprecated source overload");
}

#[test]
fn unresolved_parameter_types_do_not_create_a_guessed_overload_clash() {
    let source = r#"
class C {
    fun f(value: DefinitelyMissing): Int = 1
    fun f(value: DefinitelyMissing): Long = 2L
}
"#;

    let result = diagnostics(source);
    common::expect_identical_rejection(&result, "unresolved-only member signatures");
}

#[test]
fn early_method_annotation_fold_does_not_consume_parameter_annotations() {
    let source = r#"
@Target(AnnotationTarget.FUNCTION, AnnotationTarget.VALUE_PARAMETER)
@Retention(AnnotationRetention.BINARY)
annotation class Mark(val value: String)

interface Listener<T> {
    @Mark("function")
    fun <R : Any> onEvent(@Mark("parameter") value: T?, fallback: R? = null): R? = fallback
}
fun box(): String = "OK"
"#;

    let result = diagnostics(source);
    assert_eq!(result.reference_code, 0, "{}", result.reference_stderr);
    assert_eq!(
        result.krusty_code, 0,
        "{}{}",
        result.krusty_stdout, result.krusty_stderr
    );
    common::expect_box_ok_with_stdlib(source, "method and parameter annotation fragments");
}
