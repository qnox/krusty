//! A SAM constructor's lambda carries the interface name as its implicit label.
//!
//! `Runnable { … return@Runnable … }` converts a lambda through a SAM constructor, and Kotlin labels
//! that lambda with the interface's own name — the same rule that labels `forEach { … }`'s lambda
//! `forEach`. krusty registered no label for this shape, so the return had nothing to denote:
//!
//! ```text
//! error: return label 'Runnable' does not denote an enclosing lambda
//! ```
//!
//! It applies to a Kotlin `fun interface` and to a Java functional interface alike, since both reach
//! the same SAM-constructor conversion.

use super::common;

/// The failing shape, on a Kotlin `fun interface`.
#[test]
fn a_kotlin_fun_interface_constructor_labels_its_lambda() {
    const MAIN: &str = "fun interface Handler {\n\
\x20   fun handle(value: Int): Unit\n\
}\n\
\n\
val seen = StringBuilder()\n\
\n\
fun make(skip: Boolean): Handler =\n\
\x20   Handler { value ->\n\
\x20       if (skip) return@Handler\n\
\x20       seen.append(value)\n\
\x20   }\n\
fun box(): String {\n\
\x20   make(false).handle(1)\n\
\x20   make(true).handle(2)\n\
\x20   make(false).handle(3)\n\
\x20   return if (seen.toString() == \"13\") \"OK\" else \"FAIL: \" + seen\n\
}\n";
    common::expect_box_ok_files_with_stdlib(&[("Main.kt", MAIN)], "fun_interface_lambda_label");
}

/// The same through a JAVA functional interface, which reaches the conversion by the same route.
#[test]
fn a_java_functional_interface_constructor_labels_its_lambda() {
    const MAIN: &str = "val seen = StringBuilder()\n\
\n\
fun make(skip: Boolean): Runnable =\n\
\x20   Runnable {\n\
\x20       if (skip) return@Runnable\n\
\x20       seen.append(\"r\")\n\
\x20   }\n\
fun box(): String {\n\
\x20   make(false).run()\n\
\x20   make(true).run()\n\
\x20   make(false).run()\n\
\x20   return if (seen.toString() == \"rr\") \"OK\" else \"FAIL: \" + seen\n\
}\n";
    common::expect_box_ok_files_with_stdlib(&[("Main.kt", MAIN)], "java_interface_lambda_label");
}

/// A SAM lambda that returns a VALUE through its label, so the label must carry the result too, not
/// just act as a jump.
#[test]
fn a_labeled_return_through_a_sam_constructor_carries_its_value() {
    const MAIN: &str = "fun interface Mapper {\n\
\x20   fun map(value: Int): Int\n\
}\n\
\n\
fun make(double: Boolean): Mapper =\n\
\x20   Mapper { value ->\n\
\x20       if (!double) return@Mapper value\n\
\x20       value * 2\n\
\x20   }\n\
fun box(): String {\n\
\x20   if (make(false).map(5) != 5) return \"FAIL: passthrough\"\n\
\x20   if (make(true).map(5) != 10) return \"FAIL: doubled\"\n\
\x20   return \"OK\"\n\
}\n";
    common::expect_box_ok_files_with_stdlib(&[("Main.kt", MAIN)], "sam_label_value");
}

/// A label that denotes nothing is still rejected — registering the interface name must not make
/// every label resolve. Both compilers' output is asserted.
#[test]
fn an_unknown_label_is_still_rejected() {
    const MAIN: &str = "fun interface Handler {\n\
\x20   fun handle(value: Int): Unit\n\
}\n\
\n\
fun make(): Handler = Handler { return@Missing }\n";
    let result = common::compiler_diagnostics(&[("Main.kt", MAIN)], &[]);
    assert_ne!(
        result.reference_code, 0,
        "kotlinc must reject an unknown label: {}",
        result.reference_stderr
    );
    assert_ne!(
        result.krusty_code, 0,
        "krusty accepted an unknown label: {}{}",
        result.krusty_stdout, result.krusty_stderr
    );
}
