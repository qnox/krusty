//! An inline function that invokes its lambda parameter from inside a lambda it passes to another
//! inline function leaves the caller's literal as a capture of that inner lambda. kotlinc inlines
//! the inner call into the function first, so the literal's body replaces each invocation: a
//! `return` in it leaves the caller, and its own captures read the caller's values. The inline
//! functions are this repository's own, the inner one compiled by the reference compiler.

use super::common;

const LIB: &str = r#"
inline fun <R> runIt(block: () -> R): R = block()
"#;

const NON_LOCAL_RETURN: &str = r#"
inline fun each(action: (Int) -> Unit) {
    var index = 1
    while (index <= 2) {
        runIt { action(index) }
        index++
    }
}

fun box(): String {
    each { if (it == 2) return "OK" }
    return "FAIL"
}
"#;

const CAPTURED_VALUE: &str = r#"
inline fun total(transform: (Int) -> Int): Int {
    var sum = 0
    var index = 1
    while (index <= 3) {
        runIt { sum += transform(index) }
        index++
    }
    return sum
}

fun box(): String {
    val factor = 2
    val result = total { it * factor }
    return if (result == 12) "OK" else "FAIL $result"
}
"#;

#[test]
fn a_captured_literal_returns_from_its_caller() {
    assert_eq!(
        common::expect_box_run_against_kotlinc(LIB, NON_LOCAL_RETURN)
            .expect("reference kotlinc is provisioned"),
        "OK"
    );
}

#[test]
fn a_captured_literal_reads_its_callers_captures() {
    assert_eq!(
        common::expect_box_run_against_kotlinc(LIB, CAPTURED_VALUE)
            .expect("reference kotlinc is provisioned"),
        "OK"
    );
}
