//! How a classpath `inline` function's body expands the lambdas it invokes, compared with kotlinc.
//! The inline functions are this repository's own, compiled by the reference compiler.

use super::common;

const LIB: &str = r#"
package lib

inline fun twiceOf(x: Int, f: (Int) -> Int): Int = f(f(x))

inline fun thriceOf(x: Long, f: (Long) -> Long): Long = f(f(f(x)))
"#;

const MAIN: &str = r#"
import lib.*

fun doubled(x: Int, k: Int): Int = twiceOf(x) { it * k }

fun tripled(x: Long, k: Long): Long = thriceOf(x) { it + k }

fun box(): String {
    if (doubled(3, 2) != 12) return "FAIL doubled: " + doubled(3, 2)
    if (tripled(1L, 2L) != 7L) return "FAIL tripled: " + tripled(1L, 2L)
    return "OK"
}
"#;

#[test]
fn repeated_lambda_invokes_run_like_the_reference_compiler() {
    let output = common::expect_box_run_against_ref("inline_lambda_expansion", LIB, MAIN)
        .expect("reference kotlinc is provisioned");
    assert_eq!(output, "OK");
}

/// Each invoke of the same lambda stores its argument in the slot the previous one used: kotlinc's
/// `InstructionAdapter.store` does not advance the inliner's next free local.
#[test]
fn every_invoke_of_a_lambda_reuses_its_argument_slots() {
    let classes = common::classes_against_kotlinc_lib("Expansion", &[("Lib.kt", LIB)], MAIN)
        .expect("reference kotlinc is provisioned");
    assert_eq!(
        classes.reference.keys().collect::<Vec<_>>(),
        ["ExpansionKt"]
    );
    let differences = classes.code_differences();
    assert!(differences.is_empty(), "{}", differences.join("\n\n"));
}
