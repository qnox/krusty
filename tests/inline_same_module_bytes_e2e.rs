//! A same-module inline function with no function-typed parameter is inlined from its compiled
//! body, as kotlinc's `IrSourceCompilerForInline` hands its `MethodNode` to the inliner: the body
//! keeps its erased type parameters, so a generic argument is stored boxed into an `Object`
//! temporary and the caller narrows the erased result with a `checkcast` after the body ends.

use super::common;

fn assert_like_kotlinc(sources: &[(&str, &str)]) {
    let classes = common::classes_against_kotlinc_module(sources);
    let differences = classes.differences();
    assert!(differences.is_empty(), "{}", differences.join("\n\n"));
}

const GENERIC: &str = r#"inline fun <T> buzz(x: T): T {
    log("buzz($x)")
    return x
}

var LOG = ""

fun log(string: String) {
    LOG += "$string;"
}

fun sum(x: Int, y: Int): Int = x + y

fun box(): String {
    if (sum(1, buzz(2)) != 3) return "fail"
    buzz("discarded")
    return if (LOG == "buzz(2);buzz(discarded);") "OK" else LOG
}
"#;

#[test]
fn a_same_file_generic_inline_call_boxes_its_argument_into_an_erased_temporary() {
    assert_like_kotlinc(&[("Generic.kt", GENERIC)]);
}

#[test]
fn a_same_file_generic_inline_call_runs() {
    assert_eq!(common::expect_box_run_with_stdlib(GENERIC, "Generic"), "OK");
}

const LIBRARY: &str = r#"inline fun <T> buzz(x: T): T {
    log("buzz($x)")
    return x
}

private var LOG = ""

fun log(string: String) {
    LOG += "$string;"
}

fun pullLog(): String {
    val string = LOG
    LOG = ""
    return string
}
"#;

const MAIN: &str = r#"fun <T> fizz(x: T): T {
    log("fizz($x)")
    return x
}

fun sum(x: Int, y: Int): Int {
    log("sum($x, $y)")
    return x + y
}

fun box(): String {
    if (sum(fizz(1), buzz(2)) != 3) return "fail"
    return if (pullLog() == "fizz(1);buzz(2);sum(1, 2);") "OK" else "log"
}
"#;

#[test]
fn a_cross_file_generic_inline_call_is_inlined_from_the_compiled_body() {
    assert_like_kotlinc(&[("lib.kt", LIBRARY), ("main.kt", MAIN)]);
}

#[test]
fn a_cross_file_inline_call_declared_after_its_caller_is_inlined_from_the_compiled_body() {
    assert_like_kotlinc(&[("main.kt", MAIN), ("lib.kt", LIBRARY)]);
}

const VALUE_CLASS_LIBRARY: &str = r#"inline fun <T> inlinedId(x: T): T = x
inline fun <T> T.inlinedIdExtension(): T = this
"#;

const VALUE_CLASS_MAIN: &str = r#"@JvmInline
value class Foo(val value: Int)

fun <T> T.idExtension(): T = this

fun test(f: Foo): Int {
    inlinedId(f)
    f.inlinedIdExtension()
    val a = inlinedId(f)
    val b = f.inlinedIdExtension()
    return a.value + b.value
}
"#;

// The value class is boxed into the erased temporary, and the erased result unboxed after the body
// (or discarded): the call keeps its record when a representation wrapper takes its place.
#[test]
fn a_value_class_argument_is_boxed_into_the_erased_temporary() {
    assert_like_kotlinc(&[
        ("lib.kt", VALUE_CLASS_LIBRARY),
        ("main.kt", VALUE_CLASS_MAIN),
    ]);
}

const CHAINED_MAIN: &str = r#"fun <T> T.idExtension(): T = this

fun chained(s: String): String = inlinedId(s).idExtension()
"#;

// The inlined body runs under the callee's lines, so the call that consumes its result marks the
// caller's line again at its invoke.
#[test]
fn a_call_on_an_inlined_result_marks_its_line_again() {
    assert_like_kotlinc(&[("lib.kt", VALUE_CLASS_LIBRARY), ("main.kt", CHAINED_MAIN)]);
}

const ENUM_WHEN_LIBRARY: &str = r#"enum class Color { RED, GREEN, BLUE }

inline fun describe(color: Color): String = when (color) {
    Color.RED -> "r"
    Color.GREEN -> "g"
    Color.BLUE -> "b"
}

inline fun describeOrNone(color: Color?): String = when (color) {
    Color.RED -> "r"
    null -> "none"
    else -> "other"
}
"#;

const ENUM_WHEN_MAIN: &str = r#"fun box(): String {
    val s = describe(Color.GREEN) + describe(Color.BLUE) + describeOrNone(null) +
        describeOrNone(Color.RED) + describeOrNone(Color.GREEN)
    return if (s == "gbnonerother") "OK" else s
}
"#;

// A subject `when` over an enum reads the declaring class's `$WhenMappings` table, whose keys only
// that class's emission assigns, so the call is expanded from the callee's IR template instead.
#[test]
fn a_cross_file_inline_call_with_an_enum_subject_when_runs() {
    common::expect_box_ok_files_with_stdlib(
        &[("lib.kt", ENUM_WHEN_LIBRARY), ("main.kt", ENUM_WHEN_MAIN)],
        "EnumWhenCrossFile",
    );
}

const ENUM_WHEN_SAME_FILE: &str = r#"enum class Shade { LIGHT, DARK }

inline fun tone(shade: Shade): String = when (shade) {
    Shade.LIGHT -> "l"
    Shade.DARK -> "d"
}

fun box(): String {
    val s = tone(Shade.DARK) + tone(Shade.LIGHT)
    return if (s == "dl") "OK" else s
}
"#;

#[test]
fn a_same_file_inline_call_with_an_enum_subject_when_runs() {
    assert_eq!(
        common::expect_box_run_with_stdlib(ENUM_WHEN_SAME_FILE, "EnumWhenSameFile"),
        "OK"
    );
}
