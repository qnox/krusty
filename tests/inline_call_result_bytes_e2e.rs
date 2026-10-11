//! A same-file inline call's result is adapted by its caller after the inlined body ends, as
//! kotlinc's inliner leaves it: the `$i$f$` marker and the inlined parameter locals end at the
//! body's end label, before the `checkcast` that narrows an erased type-parameter result, the
//! `Unit` a value-position `Unit` lambda result materializes, or the `pop` of a discarded result.

use super::common;

fn assert_like_kotlinc(file: &str, source: &str) {
    let classes = common::classes_against_kotlinc_module(&[(file, source)]);
    let differences = classes.differences();
    assert!(differences.is_empty(), "{}", differences.join("\n\n"));
}

#[test]
fn an_erased_safe_call_result_is_narrowed_after_the_inlined_body() {
    assert_like_kotlinc(
        "Narrowed.kt",
        r#"
inline fun <T, R> T.pass(f: (T) -> R): R = f(this)

fun use(a: String?, f: (String) -> String): String? {
    val r = a?.pass(f)
    return r
}
"#,
    );
}

#[test]
fn a_unit_safe_call_result_is_materialized_after_the_inlined_body() {
    assert_like_kotlinc(
        "UnitResult.kt",
        r#"
inline fun <T, R> T.pass(f: (T) -> R): R = f(this)

fun f(s: String) { }

fun use(a: String?): Unit? {
    val r = a?.pass { f(it) }
    return r
}
"#,
    );
}

const RECORDED: &str = r#"
inline fun <T, R> T.pass(f: (T) -> R): R = f(this)

var seen = ""

fun record(text: String) {
    seen += text
}

fun box(): String {
    val a: String? = "OK"
    val r = a?.pass { record(it) }
    return if (r == Unit) seen else "FAIL"
}
"#;

#[test]
fn a_recorded_unit_safe_call_result_runs() {
    assert_eq!(
        common::expect_box_run_with_stdlib(RECORDED, "Recorded"),
        "OK"
    );
}

#[test]
fn a_recorded_unit_safe_call_result_is_materialized_like_the_reference_compiler() {
    assert_like_kotlinc("Recorded.kt", RECORDED);
}

#[test]
fn a_discarded_inline_call_result_is_popped_after_the_inlined_body() {
    assert_like_kotlinc(
        "Discarded.kt",
        r#"
inline fun <T, R> T.pass(f: (T) -> R): R = f(this)

fun stmt(a: String, f: (String) -> String) {
    a.pass(f)
    a.pass(f)
}

inline fun twice(f: () -> Int): Int = f() + f()

fun t(f: () -> Int) {
    twice(f)
    twice(f)
}
"#,
    );
}
