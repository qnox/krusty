//! A `return@label` carries its lambda's expected result type.
//!
//! An inline lambda's TAIL expression is checked against the result type the call site expects, so
//! `run { … ; emptyList() }` infers `List<Long>` from the declared result. A labelled return out of
//! the same lambda was checked with no expectation at all, so `return@run emptyList()` inferred an
//! unconstrained element type. Merging that with the tail's `List<Long>` produced `Any`, and the
//! declaration was then rejected against its own declared type:
//! `type mismatch: inferred type is Any but List<Long> was expected`.
//!
//! Both exits of a lambda are the same result position and must be judged against the same
//! expectation.

use super::common;

/// The failing shape: an early labelled return whose value needs the expectation to infer.
#[test]
fn a_labeled_return_infers_from_the_lambdas_expected_result() {
    const MAIN: &str = "fun pick(empty: Boolean): List<Long> =\n\
\x20   run {\n\
\x20       if (empty) return@run emptyList()\n\
\x20       listOf(1L)\n\
\x20   }\n\
fun box(): String {\n\
\x20   if (pick(true) != emptyList<Long>()) return \"FAIL: empty\"\n\
\x20   if (pick(false) != listOf(1L)) return \"FAIL: present\"\n\
\x20   return \"OK\"\n\
}\n";
    common::expect_box_ok_files_with_stdlib(&[("Main.kt", MAIN)], "labeled_return_expected");
}

/// The corpus shape this came from: a labelled return out of a receiver lambda, guarding an early
/// exit before the real result. `let` and `use` take the same path as `run`.
#[test]
fn a_labeled_return_from_a_receiver_lambda_infers_the_same_way() {
    const MAIN: &str = "class Source(val rows: List<String>?)\n\
fun read(source: Source): List<String> =\n\
\x20   source.let { s ->\n\
\x20       val rows = s.rows ?: return@let emptyList()\n\
\x20       rows.map { it.trim() }\n\
\x20   }\n\
fun box(): String {\n\
\x20   if (read(Source(null)) != emptyList<String>()) return \"FAIL: absent\"\n\
\x20   if (read(Source(listOf(\" a \"))) != listOf(\"a\")) return \"FAIL: present\"\n\
\x20   return \"OK\"\n\
}\n";
    common::expect_box_ok_files_with_stdlib(&[("Main.kt", MAIN)], "labeled_return_receiver_lambda");
}

/// The spellings that already worked stay working, so the expectation cannot start depending on
/// which exit of the lambda produced the value.
#[test]
fn the_other_lambda_exit_spellings_still_infer() {
    const MAIN: &str = "fun explicit(empty: Boolean): List<Long> =\n\
\x20   run { if (empty) return@run emptyList<Long>(); listOf(1L) }\n\
fun tail(empty: Boolean): List<Long> =\n\
\x20   run { if (empty) emptyList() else listOf(1L) }\n\
fun swapped(empty: Boolean): List<Long> =\n\
\x20   run { if (!empty) return@run listOf(1L); emptyList() }\n\
fun box(): String {\n\
\x20   if (explicit(true) != emptyList<Long>()) return \"FAIL: explicit\"\n\
\x20   if (tail(false) != listOf(1L)) return \"FAIL: tail\"\n\
\x20   if (swapped(true) != emptyList<Long>()) return \"FAIL: swapped\"\n\
\x20   return \"OK\"\n\
}\n";
    common::expect_box_ok_files_with_stdlib(&[("Main.kt", MAIN)], "lambda_exit_spellings");
}

/// A labelled return whose value genuinely does not fit the lambda's result is still rejected —
/// the expectation must inform inference, not paper over a mismatch. Both compilers' output is
/// asserted.
#[test]
fn a_labeled_return_of_the_wrong_type_is_still_rejected() {
    const MAIN: &str = "fun pick(empty: Boolean): List<Long> =\n\
\x20   run {\n\
\x20       if (empty) return@run \"not a list\"\n\
\x20       listOf(1L)\n\
\x20   }\n";
    let result = common::compiler_diagnostics(&[("Main.kt", MAIN)], &[]);
    assert_ne!(
        result.reference_code, 0,
        "kotlinc must reject a String returned from a List<Long> lambda: {}",
        result.reference_stderr
    );
    assert_ne!(
        result.krusty_code, 0,
        "krusty accepted a mismatched labelled return: {}{}",
        result.krusty_stdout, result.krusty_stderr
    );
}
