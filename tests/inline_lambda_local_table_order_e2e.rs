//! The local variable table of an inlined lambda keeps kotlinc's order. The lambda's method closes
//! its outermost scope at its end: first the `$i$a$` marker it declared on entry, then the locals
//! the body declares directly, then its parameters. A nested block's locals were closed where the
//! block ended, before all of those. The inline function is this repository's own, compiled by
//! the reference compiler.

use super::common;

const LIB: &str = r#"
inline fun <T, R> withIt(value: T, block: (T) -> R): R = block(value)
"#;

const MAIN: &str = r#"
var largest = 0

fun scaled(factor: Int): Int {
    val scaled = withIt(factor) {
        val product = it * 21
        if (product > largest) {
            val bound = product + 1
            largest = bound
        }
        product
    }
    return scaled
}

fun box(): String {
    val scaled = scaled(2)
    return if (scaled == 42 && largest == 43) "OK" else "FAIL $scaled $largest"
}
"#;

#[test]
fn an_inlined_lambda_with_locals_runs() {
    assert_eq!(
        common::expect_box_run_against_kotlinc(LIB, MAIN)
            .expect("reference kotlinc is provisioned"),
        "OK"
    );
}

#[test]
fn an_inlined_lambda_orders_its_locals_like_the_reference_compiler() {
    let classes = common::classes_against_kotlinc_lib("Main", &[("Lib.kt", LIB)], MAIN)
        .expect("reference kotlinc is provisioned");
    let differences = classes.differences();
    assert!(differences.is_empty(), "{}", differences.join("\n\n"));
}
