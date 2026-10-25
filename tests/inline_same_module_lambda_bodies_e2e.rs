//! A same-module inline function whose body declares a lambda is inlined from its compiled body,
//! as kotlinc's `IrSourceCompilerForInline` compiles the function with its file's classes. The
//! lambda's class is the declaring file's; a call site reuses it unless the call must specialize
//! it for a reified type argument, when the call site regenerates it under its own name.

use super::common;

fn assert_like_kotlinc(sources: &[(&str, &str)]) {
    let classes = common::classes_against_kotlinc_module(sources);
    let differences = classes.differences();
    assert!(differences.is_empty(), "{}", differences.join("\n\n"));
}

/// A capturing lambda the call needs no specialization of is constructed from its declaring
/// class; one that tests a reified type argument is regenerated for the call.
const SAME_FILE: &str = r#"fun <T> Array<T>.keep(predicate: (T) -> Boolean): List<T> = filter(predicate)

fun call(f: () -> String) = f()

inline fun <reified T> instancesOf(values: Array<Any>): List<T> =
    values.keep { it is T }.map { it as T }

inline fun exclaim(s: String): String = call { s + "!" }

fun box(): String {
    val values: Array<Any> = arrayOf("O", 1)
    val strings = instancesOf<String>(values)
    if (strings != listOf("O")) return "strings $strings"
    if (exclaim("K") != "K!") return "exclaim"
    return strings[0] + "K"
}
"#;

#[test]
fn a_lambda_in_a_same_file_inline_body_is_reused_or_regenerated_like_kotlinc() {
    assert_like_kotlinc(&[("Lambdas.kt", SAME_FILE)]);
}

#[test]
fn a_lambda_in_a_same_file_inline_body_runs() {
    assert_eq!(
        common::expect_box_run_with_stdlib(SAME_FILE, "Lambdas"),
        "OK"
    );
}

/// The declaring file writes the lambda's class. The caller's file compiles it under that file's
/// name and source only to read it, and regenerates the reified one for each call.
const OTHER_FILE_DECLARATION: &str = r#"fun <T> Array<T>.keep(predicate: (T) -> Boolean): List<T> = filter(predicate)

fun call(f: () -> String) = f()

inline fun <reified T> instancesOf(values: Array<Any>): List<T> =
    values.keep { it is T }.map { it as T }

inline fun exclaim(s: String): String = call { s + "!" }
"#;

const OTHER_FILE_CALLER: &str = r#"fun box(): String {
    val values: Array<Any> = arrayOf("O", 1, 2L)
    if (instancesOf<Int>(values) != listOf(1)) return "ints"
    val strings = instancesOf<String>(values)
    if (exclaim("K") != "K!") return "exclaim"
    return strings[0] + "K"
}
"#;

#[test]
fn a_lambda_in_another_files_inline_body_is_reused_or_regenerated_like_kotlinc() {
    assert_like_kotlinc(&[
        ("Declaration.kt", OTHER_FILE_DECLARATION),
        ("Caller.kt", OTHER_FILE_CALLER),
    ]);
}

#[test]
fn a_lambda_in_another_files_inline_body_runs() {
    common::expect_box_ok_files_with_stdlib(
        &[
            ("Declaration.kt", OTHER_FILE_DECLARATION),
            ("Caller.kt", OTHER_FILE_CALLER),
        ],
        "Caller",
    );
}

/// A lambda class whose methods use a reified parameter is marked where its instance is loaded,
/// not in its own initializer, so the call regenerates it and a caller that reuses the class never
/// runs the marker.
const REIFIED_LAMBDA: &str = r#"fun call(f: () -> String) = f()

inline fun <reified T> name(): String = call { T::class.simpleName!! }

fun box(): String = if (name<String>() == "String") "OK" else "fail"
"#;

#[test]
fn a_reified_lambda_class_is_marked_where_its_instance_is_loaded() {
    assert_like_kotlinc(&[("Marker.kt", REIFIED_LAMBDA)]);
}

#[test]
fn a_reified_lambda_class_regenerated_from_a_compiled_body_runs() {
    assert_eq!(
        common::expect_box_run_with_stdlib(REIFIED_LAMBDA, "Marker"),
        "OK"
    );
}

/// A default lambda that uses a reified parameter is marked at its load as well: its class's
/// initializer carries no marker, so the call that inlines the default runs it unspecialized.
const REIFIED_DEFAULT_LAMBDA: &str = r#"inline fun <reified T> name(f: () -> String = { T::class.simpleName!! }): String = f()

fun box(): String = if (name<String>() == "String") "OK" else "fail"
"#;

#[test]
fn a_reified_default_lambda_runs() {
    assert_eq!(
        common::expect_box_run_with_stdlib(REIFIED_DEFAULT_LAMBDA, "Default"),
        "OK"
    );
}

/// A `when` branch whose class already is the `when`'s, but whose type names a reified parameter,
/// casts to that class again, so the join a call specializes still verifies; a branch coerced
/// from a generic call's erased result casts only once.
const REIFIED_WHEN_JOIN: &str = r#"inline fun <reified T : Number> T.self(): T = this

inline fun <reified T : Number> List<T>.pick(first: Boolean): String =
    T::class.simpleName + (if (first) this[0].self() else this[1]).toInt()

fun box(): String {
    val values = listOf(1L, 2L)
    if (values.pick(true) != "Long1") return "first"
    return if (values.pick(false) == "Long2") "OK" else "second"
}
"#;

#[test]
fn a_reified_when_branch_of_the_joins_class_casts_again_like_kotlinc() {
    assert_like_kotlinc(&[("Join.kt", REIFIED_WHEN_JOIN)]);
}

#[test]
fn a_reified_when_join_inlined_from_a_compiled_body_runs() {
    assert_eq!(
        common::expect_box_run_with_stdlib(REIFIED_WHEN_JOIN, "Join"),
        "OK"
    );
}

/// A lambda class whose own function type names a reified parameter guards its singleton
/// initializer. A copy drops that marker from its initializer, and a copy made inside another
/// reified inline function keeps the marker at its load for that function's callers.
const REIFIED_FUNCTION_TYPE: &str = r#"inline fun <reified T> make(): () -> T = { "OK" as T }

inline fun <reified U> again(): () -> U = make<U>()

fun box(): String {
    if (make<String>()() != "OK") return "direct"
    return again<String>()()
}
"#;

#[test]
fn a_reified_function_type_lambda_is_regenerated_with_its_initializer_unguarded() {
    let classes = common::classes_against_kotlinc_module(&[("Typed.kt", REIFIED_FUNCTION_TYPE)]);
    assert_eq!(
        classes.krusty.keys().collect::<Vec<_>>(),
        classes.reference.keys().collect::<Vec<_>>()
    );
    assert_eq!(
        common::expect_box_run_with_stdlib(REIFIED_FUNCTION_TYPE, "Typed"),
        "OK"
    );
}

/// A reified lambda nested in another lambda of the callee is regenerated as the copy's next
/// object, so the copy's method that loads it uses the specialized class.
const REIFIED_NESTED_LAMBDA: &str = r#"interface Item
class Token : Item
class Root : Item
var check: (Item) -> Boolean = { false }
inline fun <reified T : Item> define() {
    check = {
        val nested = { value: Item -> value is T }
        nested(it)
    }
}
fun box(): String {
    define<Token>()
    return if (check(Token()) && !check(Root())) "OK" else "fail"
}
"#;

#[test]
fn a_reified_lambda_nested_in_a_regenerated_lambda_is_regenerated_too() {
    let classes = common::classes_against_kotlinc_module(&[("Nested.kt", REIFIED_NESTED_LAMBDA)]);
    assert_eq!(
        classes.krusty.keys().collect::<Vec<_>>(),
        classes.reference.keys().collect::<Vec<_>>()
    );
    assert_eq!(
        common::expect_box_run_with_stdlib(REIFIED_NESTED_LAMBDA, "Nested"),
        "OK"
    );
}
