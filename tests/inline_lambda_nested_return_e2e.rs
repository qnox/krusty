//! A `return@outer` written inside a lambda nested in another inlined lambda leaves only the outer
//! lambda: inlined into its caller, it exits the outer lambda's invocation and the outer callee
//! goes on. It never returns from the function both are inlined into. The inline functions are
//! this repository's own, compiled by the reference compiler.

use super::common;

const LIB: &str = r#"
inline fun <R> runIt(block: () -> R): R = block()

inline fun times(count: Int, action: (Int) -> Unit) {
    for (index in 0 until count) action(index)
}
"#;

const MAIN: &str = r#"
var counter = 0

fun countAll() {
    times(10) {
        runIt {
            counter++
            return@times
        }
    }
}

fun box(): String {
    countAll()
    return if (counter == 10) "OK" else "FAIL"
}
"#;

#[test]
fn a_return_to_the_outer_lambda_leaves_only_that_lambda() {
    assert_eq!(
        common::expect_box_run_against_kotlinc(LIB, MAIN).expect("reference kotlinc is provisioned"),
        "OK"
    );
}
