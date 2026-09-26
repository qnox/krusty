//! A literal lambda passed to a classpath `inline` function's `crossinline` parameter is inlined
//! where the body invokes it, the same as one passed to a plain inline parameter: kotlinc's
//! `isInlineParameter()` is "function type and not `noinline`". The inline functions are this
//! repository's own, compiled by the reference compiler.

use super::common;

const LIB: &str = r#"
package lib

inline fun applied(x: Int, crossinline f: (Int) -> Int): Int = f(x)

inline fun <T> produced(crossinline f: () -> T): T = f()

inline fun twice(crossinline f: () -> Unit) {
    f()
    f()
}
"#;

const MAIN: &str = r#"
import lib.*

fun increased(x: Int, k: Int): Int = applied(x) { it + k }

fun greeting(name: String): String = produced { "hi " + name }

fun counted(): Int {
    var n = 0
    twice { n += 1 }
    return n
}

fun box(): String {
    if (increased(40, 2) != 42) return "FAIL increased: " + increased(40, 2)
    if (greeting("k") != "hi k") return "FAIL greeting: " + greeting("k")
    if (counted() != 2) return "FAIL counted: " + counted()
    return "OK"
}
"#;

#[test]
fn crossinline_lambdas_run_like_the_reference_compiler() {
    let output = common::expect_box_run_against_ref("crossinline_lambda_inlining", LIB, MAIN)
        .expect("reference kotlinc is provisioned");
    assert_eq!(output, "OK");
}

/// The lambdas' bodies stand in the caller where kotlinc puts them, with its `$i$a$` markers,
/// lines and locals, and a `var` the lambda changes stays a plain local rather than a `Ref`.
/// Constant-pool order is left out: the optimizer still lays the pool out before its passes.
#[test]
fn crossinline_lambdas_are_inlined_where_kotlinc_inlines_them() {
    let classes = common::classes_against_kotlinc_lib("Crossinline", &[("Lib.kt", LIB)], MAIN)
        .expect("reference kotlinc is provisioned");
    assert_eq!(
        classes.reference.keys().collect::<Vec<_>>(),
        ["CrossinlineKt"]
    );
    let differences = classes.code_differences();
    assert!(differences.is_empty(), "{}", differences.join("\n\n"));
}

const MIXED_LIB: &str = r#"
package lib

inline fun guarded(flag: Boolean, onTrue: () -> Unit, crossinline onFalse: () -> Int): Int {
    if (flag) onTrue()
    return onFalse()
}

inline fun combined(x: Int, crossinline f: (Int) -> Int, noinline g: (Int) -> Int): Int {
    val first = f(x)
    return first + g(x)
}
"#;

const MIXED_MAIN: &str = r#"
import lib.*

fun probe(flag: Boolean, k: Int): Int = guarded(flag, { return -1 }) { k + 1 }

fun mixed(x: Int, k: Int): Int = combined(x, { it * k }) { it + 1 }

fun box(): String {
    if (probe(true, 1) != -1) return "FAIL probe true: " + probe(true, 1)
    if (probe(false, 1) != 2) return "FAIL probe false: " + probe(false, 1)
    if (mixed(3, 2) != 10) return "FAIL mixed: " + mixed(3, 2)
    return "OK"
}
"#;

#[test]
fn mixed_modifier_calls_run_like_the_reference_compiler() {
    let output =
        common::expect_box_run_against_ref("crossinline_mixed_modifiers", MIXED_LIB, MIXED_MAIN)
            .expect("reference kotlinc is provisioned");
    assert_eq!(output, "OK");
}

/// Each literal takes its own parameter's modifier: beside a `noinline` literal, which stays the
/// function object the body invokes, the `crossinline` one is still inlined.
#[test]
fn a_noinline_literal_leaves_the_crossinline_one_inlined() {
    let main = r#"
import lib.*

fun mixed(x: Int, k: Int): Int = combined(x, { it * k }) { it + 1 }
"#;
    let classes = common::classes_against_kotlinc_lib("Mixed", &[("Lib.kt", MIXED_LIB)], main)
        .expect("reference kotlinc is provisioned");
    assert_eq!(classes.reference.keys().collect::<Vec<_>>(), ["MixedKt"]);
    let differences = classes.code_differences();
    assert!(differences.is_empty(), "{}", differences.join("\n\n"));
}

/// A plain literal that leaves by a non-local `return` keeps the call on the byte splice, and the
/// splice expands the `crossinline` literal beside it exactly as it expands a plain one: the
/// caller's code is the same with or without the modifier.
#[test]
fn the_splice_expands_a_crossinline_literal_like_a_plain_one() {
    let main = r#"
import lib.*

fun probe(flag: Boolean, k: Int): Int = guarded(flag, { return -1 }) { k + 1 }
"#;
    let crossinline =
        common::classes_against_kotlinc_lib("Guarded", &[("Lib.kt", MIXED_LIB)], main)
            .expect("reference kotlinc is provisioned");
    let plain_lib = MIXED_LIB.replace("crossinline onFalse", "onFalse");
    let plain = common::classes_against_kotlinc_lib("Guarded", &[("Lib.kt", &plain_lib)], main)
        .expect("reference kotlinc is provisioned");
    assert_eq!(crossinline.krusty.keys().collect::<Vec<_>>(), ["GuardedKt"]);
    assert_eq!(
        crossinline.krusty_code("GuardedKt"),
        plain.krusty_code("GuardedKt")
    );
}
