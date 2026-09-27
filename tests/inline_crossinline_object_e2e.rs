//! An anonymous object that a classpath `inline` function's body creates around one of its
//! `crossinline` lambdas is regenerated for each call site with the lambda's body inlined into the
//! copy's methods, the way kotlinc's `AnonymousObjectTransformer` does: the lambda's field is gone,
//! its captured values become the copy's `$<name>$inlined` fields, and the call site passes them
//! to the copy's constructor instead of a lambda.

use super::common;

const LIB: &str = r#"
package lib

interface Greeter {
    fun greet(): String
}

interface Counter {
    fun count(step: Int): Long
    fun name(prefix: String): String
}

inline fun greeting(crossinline make: (String) -> String): Greeter = object : Greeter {
    override fun greet(): String = make("hi")
}

inline fun counting(label: String, crossinline next: (Int) -> Long): Counter = object : Counter {
    override fun count(step: Int): Long = next(step) + next(step + 1)
    override fun name(prefix: String): String = prefix + label
}

inline fun twoWays(crossinline first: () -> String, crossinline second: () -> String): Greeter =
    object : Greeter {
        override fun greet(): String = first() + second()
    }
"#;

const MAIN: &str = r#"
import lib.*

fun captured(suffix: String): String = greeting { it + suffix }.greet()

fun counted(base: Long, scale: Int): Long = counting("c") { base + it * scale }.count(2)

fun named(): String = counting("x") { it.toLong() }.name("n")

fun both(a: String, b: String): String = twoWays({ a }, { b + "!" }).greet()

fun box(): String {
    if (captured("!") != "hi!") return "FAIL captured: " + captured("!")
    if (counted(10L, 3) != 35L) return "FAIL counted: " + counted(10L, 3)
    if (named() != "nx") return "FAIL named: " + named()
    if (both("a", "b") != "ab!") return "FAIL both: " + both("a", "b")
    return "OK"
}
"#;

#[test]
fn crossinline_objects_run_like_the_reference_compiler() {
    let output = common::expect_box_run_against_kotlinc(LIB, MAIN)
        .expect("reference kotlinc is provisioned");
    assert_eq!(output, "OK");
}

#[test]
fn crossinline_objects_are_the_reference_compilers_classes() {
    let classes = common::classes_against_kotlinc_lib("Crossinline", &[("Lib.kt", LIB)], MAIN)
        .expect("reference kotlinc is provisioned");
    assert_eq!(
        classes.reference.keys().collect::<Vec<_>>(),
        [
            "CrossinlineKt",
            "CrossinlineKt$both$$inlined$twoWays$1",
            "CrossinlineKt$captured$$inlined$greeting$1",
            "CrossinlineKt$counted$$inlined$counting$1",
            "CrossinlineKt$named$$inlined$counting$1",
        ]
    );
    let differences = classes.differences();
    assert!(differences.is_empty(), "{}", differences.join("\n\n"));
}

const HOLDER_LIB: &str = r#"
package lib

class Holder<V> {
    var held: V? = null
}

inline fun <V> Holder<V>.keep(value: V) {
    held = value
}
"#;

const HOLDER_MAIN: &str = r#"
import lib.*

fun box(): String {
    val holder = Holder<() -> String>()
    holder.keep { "OK" }
    return holder.held!!()
}
"#;

/// A literal passed where the inline function's parameter is not function-typed is an ordinary
/// value, not a lambda to inline: the body creates no object for it to be regenerated around.
#[test]
fn a_literal_for_a_value_parameter_stays_a_value() {
    let output = common::expect_box_run_against_kotlinc(HOLDER_LIB, HOLDER_MAIN)
        .expect("reference kotlinc is provisioned");
    assert_eq!(output, "OK");
}

const FIELD_MAIN: &str = r#"
import lib.*

private var greeted = ""
var named = ""

fun remembered(): String = greeting { greeted = it; named = it + "!"; greeted + named }.greet()

fun box(): String {
    if (remembered() != "hihi!") return "FAIL remembered: " + remembered()
    return "OK"
}
"#;

/// A lambda that reads or writes a property's backing field keeps working when the callee passes
/// it to an object: the field is private to the caller's class, which the object is not.
#[test]
fn a_lambda_writing_a_backing_field_runs_like_the_reference_compiler() {
    let output = common::expect_box_run_against_kotlinc(LIB, FIELD_MAIN)
        .expect("reference kotlinc is provisioned");
    assert_eq!(output, "OK");
}

const NESTED_LIB: &str = r#"
package lib

interface Source {
    fun read(): String
}

inline fun source(crossinline read: () -> String): Source = object : Source {
    override fun read(): String = read()
}

inline fun prefixed(crossinline rest: () -> String): Source {
    val x = "p"
    return source { x + rest() }
}
"#;

const NESTED_MAIN: &str = r#"
import lib.*

fun twice(x: String): String = prefixed { x + x }.read()

fun box(): String {
    if (twice("a") != "paa") return "FAIL twice: " + twice("a")
    return "OK"
}
"#;

/// The library's copy already has an `$x$inlined` field, so the call site's `x` takes the next
/// number (`addUniqueField` counts every field the original declares).
#[test]
fn a_captured_field_name_the_object_already_has_is_numbered() {
    let classes =
        common::classes_against_kotlinc_lib("Nested", &[("Lib.kt", NESTED_LIB)], NESTED_MAIN)
            .expect("reference kotlinc is provisioned");
    assert_eq!(
        classes.reference.keys().collect::<Vec<_>>(),
        ["NestedKt", "NestedKt$twice$$inlined$prefixed$1"]
    );
    let differences = classes.differences();
    assert!(differences.is_empty(), "{}", differences.join("\n\n"));
}

const CONCAT_MAIN: &str = r#"
import lib.*

fun joined(suffix: String): String = greeting { it + "-" + suffix }.greet()

fun box(): String {
    if (joined("x") != "hi-x") return "FAIL joined: " + joined("x")
    return "OK"
}
"#;

/// On JVM 9+ the lambda's concatenation is an `invokedynamic`, whose bootstrap method compiling the
/// lambda registers in the caller. The code moves into the copy, so the caller keeps neither the
/// bootstrap method nor its constants.
#[test]
fn a_bootstrap_method_the_object_only_lambda_needs_stays_out_of_the_caller() {
    let classes = common::classes_against_kotlinc_lib_target(
        "Concat",
        &[("Lib.kt", LIB)],
        CONCAT_MAIN,
        Some(11),
    )
    .expect("reference kotlinc is provisioned");
    assert_eq!(
        classes.reference.keys().collect::<Vec<_>>(),
        ["ConcatKt", "ConcatKt$joined$$inlined$greeting$1"]
    );
    let differences = classes.differences();
    assert!(differences.is_empty(), "{}", differences.join("\n\n"));
}

const MATERIALIZED_MAIN: &str = r#"
import lib.*

fun deferred(suffix: String): String = greeting {
    val tail = { suffix + "!" }
    it + tail()
}.greet()

fun box(): String {
    if (deferred("x") != "hix!") return "FAIL deferred: " + deferred("x")
    return "OK"
}
"#;

/// A lambda that would declare a method of the caller (here the class of a lambda it materializes)
/// is kept on the bridge by route planning, before any of its code is emitted.
#[test]
fn a_lambda_declaring_a_caller_member_stays_on_the_bridge() {
    let output = common::expect_box_run_against_kotlinc(LIB, MATERIALIZED_MAIN)
        .expect("reference kotlinc is provisioned");
    assert_eq!(output, "OK");
}
