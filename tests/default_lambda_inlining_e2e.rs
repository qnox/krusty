//! A call that omits an inline lambda parameter of a classpath `inline` function inlines the
//! parameter's default lambda from the class the library compiled for it, as kotlinc's
//! `DefaultLambda` does: the `$default` bridge's arm that loads the lambda's singleton goes, and
//! each `invoke` of the parameter becomes the class's `invoke` body, specialized for the call's
//! reified type arguments when the lambda uses them. The inline functions are this repository's
//! own, compiled by the reference compiler.

use super::common;

const LIB: &str = r#"
package lib

inline fun <reified T : Any> named(name: String? = null, block: T.() -> Unit = {}): String =
    T::class.simpleName + (name ?: "")

inline fun <reified T : Any> invoking(value: T, block: T.() -> Unit = {}): T {
    block(value)
    return value
}

inline fun <reified T : Any> describe(
    value: T,
    render: (T) -> String = { it.toString() + ":" + T::class.simpleName },
): String = render(value)

inline fun <reified T : Any> both(
    value: T,
    first: (T) -> String = { "a" },
    second: (T) -> String = {
        val text = "b"
        text + it
    },
): String = first(value) + second(value)
"#;

const MAIN: &str = r#"
import lib.*

fun namedDefault(): String = named<String>()

fun invoked(): String = invoking("v")

fun described(): String = describe(7)

fun mixed(): String = both("x", first = { it + "!" })

fun defaulted(): String = both(1)

fun box(): String {
    if (namedDefault() != "String") return "FAIL named: " + namedDefault()
    if (invoked() != "v") return "FAIL invoked: " + invoked()
    if (described() != "7:Int") return "FAIL described: " + described()
    if (mixed() != "x!bx") return "FAIL mixed: " + mixed()
    if (defaulted() != "ab1") return "FAIL defaulted: " + defaulted()
    return "OK"
}
"#;

/// The library's `$default` bridges are kotlinc's: each mask test stands on the signature line,
/// and a default that needs class reification opens its own line on the `needClassReification`
/// marker, the instruction an inliner reads just before the lambda's instance load.
#[test]
fn the_library_bridges_are_the_reference_compilers() {
    let classes = common::classes_against_kotlinc_module(&[("Lib.kt", LIB)]);
    let differences = classes.code_differences();
    assert!(differences.is_empty(), "{}", differences.join("\n\n"));
}

#[test]
fn default_lambdas_run_like_the_reference_compiler() {
    let output = common::expect_box_run_against_ref("default_lambda_inlining", LIB, MAIN)
        .expect("reference kotlinc is provisioned");
    assert_eq!(output, "OK");
}

/// The default lambdas' bodies stand in the caller where kotlinc puts them, with their
/// `$i$a$` markers, their own source-map entries, and the lines of the bridge arms whose code
/// went. Constant-pool order is left out: the optimizer still lays the pool out before its passes.
#[test]
fn default_lambdas_are_inlined_where_kotlinc_inlines_them() {
    let classes = common::classes_against_kotlinc_lib("DefaultLambdas", &[("Lib.kt", LIB)], MAIN)
        .expect("reference kotlinc is provisioned");
    assert_eq!(
        classes.reference.keys().collect::<Vec<_>>(),
        ["DefaultLambdasKt"]
    );
    let differences = classes.code_differences();
    assert!(differences.is_empty(), "{}", differences.join("\n\n"));
}
