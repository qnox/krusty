//! A literal lambda passed to a classpath `inline` function takes one route, planned before the
//! call is emitted: the MethodInliner port when it owns the lambda's shape, the byte splice for the
//! shapes a later stage ports (a suspending lambda, a value-class adapter, a non-local jump). The
//! inline functions are this repository's own, compiled by the reference compiler, so no library
//! function's special handling can make a test pass.

use super::common;

const LIB: &str = r#"
inline fun times(count: Int, action: (Int) -> Unit) {
    for (index in 0 until count) {
        action(index)
    }
}

inline fun <T> Iterable<T>.tally(predicate: (T) -> Boolean): Int {
    var matches = 0
    for (element in this) if (predicate(element)) matches++
    return matches
}

inline fun assemble(build: StringBuilder.() -> Unit): String {
    val builder = StringBuilder()
    builder.build()
    return builder.toString()
}

inline fun attempt(block: () -> Int): Int =
    try {
        block()
    } catch (error: IllegalStateException) {
        -1
    }

@JvmInline
value class Meters(val value: Int)

inline fun measure(step: (Meters) -> Int): Int = step(Meters(40))
"#;

const MAIN: &str = r#"
fun total(): Int {
    var sum = 0
    times(4) { sum += it }
    return sum
}

fun evens(values: List<Int>): Int = values.tally { it % 2 == 0 }

fun greeting(name: String): String = assemble {
    append("Hello, ")
    append(name)
}

fun failing(): Int = attempt { error("boom") }

fun firstOver(limit: Int): Int {
    times(10) { if (it > limit) return it }
    return -1
}

fun stretched(): Int = measure { it.value + 2 }

fun box(): String {
    if (total() != 6) return "FAIL total: " + total()
    if (evens(listOf(1, 2, 3, 4)) != 2) return "FAIL evens"
    if (greeting("route") != "Hello, route") return "FAIL greeting: " + greeting("route")
    if (failing() != -1) return "FAIL failing: " + failing()
    if (firstOver(3) != 4) return "FAIL firstOver: " + firstOver(3)
    if (stretched() != 42) return "FAIL stretched: " + stretched()
    return "OK"
}
"#;

#[test]
fn every_route_runs_like_the_reference_compiler() {
    let output = common::expect_box_run_against_ref("inline_lambda_route", LIB, MAIN)
        .expect("reference kotlinc is provisioned");
    assert_eq!(output, "OK");
}

/// The methods whose lambdas the port owns: each is kotlinc's instruction for instruction, over the
/// same slots and locals. Two methods are left out for gaps outside the route: kotlinc returns
/// from a `Unit` lambda (`greeting`) on its closing line, which common lowering does not record for
/// a lambda yet, and it keeps `failing`'s try result in its local because the catch variable's range
/// ends between the store and the load, where the temporaries pass folds them.
#[test]
fn a_lambda_the_port_owns_is_inlined_like_the_reference_compiler() {
    for method in [
        "public static final int total()",
        "public static final int evens(java.util.List<java.lang.Integer>)",
    ] {
        match common::method_code_diff_against_kotlinc(
            "InlineLambdaRoute",
            &[("Lib.kt", LIB)],
            MAIN,
            "InlineLambdaRouteKt",
            method,
        )
        .expect("reference kotlinc is provisioned")
        {
            Ok(()) => {}
            Err(difference) => panic!("{difference}"),
        }
    }
}
