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

const LAMBDA: &str = r#"inline fun <T> twice(block: () -> T): T {
    block()
    return block()
}

inline fun <T, R> T.applied(transform: (T) -> R): R = transform(this)

fun box(): String {
    var count = 0
    val o = twice {
        count += 1
        "O"
    }
    val k = "K".applied { it + it.length }
    return if (count == 2) o + k.substring(0, 1) else "fail: $count"
}
"#;

// A literal lambda argument is inlined into the invokes of the callee's compiled body, as kotlinc's
// `MethodInliner` places it: the lambda's code replaces each `invoke`, and no lambda method is left
// in the caller.
#[test]
fn a_same_file_inline_call_with_a_literal_lambda_is_inlined_from_the_compiled_body() {
    assert_like_kotlinc(&[("Lambda.kt", LAMBDA)]);
}

#[test]
fn a_same_file_inline_call_with_a_literal_lambda_runs() {
    assert_eq!(common::expect_box_run_with_stdlib(LAMBDA, "Lambda"), "OK");
}

const LAMBDA_LIBRARY: &str = r#"inline fun <T> measured(name: String, block: () -> T): T {
    log("start $name")
    val result = block()
    log("end $name")
    return result
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

const LAMBDA_MAIN: &str = r#"fun box(): String {
    val prefix = "O"
    val result = measured("ok") { prefix + "K" }
    return if (pullLog() == "start ok;end ok;") result else "log"
}
"#;

#[test]
fn a_cross_file_inline_call_with_a_capturing_lambda_is_inlined_from_the_compiled_body() {
    assert_like_kotlinc(&[("lib.kt", LAMBDA_LIBRARY), ("main.kt", LAMBDA_MAIN)]);
}

const FUNCTION_VALUE_LIBRARY: &str = r#"inline fun run(func: () -> Int): Int {
    return func()
}

inline fun <T> String.app(f: (String) -> T): T = f(this)

inline fun new(init: (Int) -> Unit): Int = 42
"#;

const FUNCTION_VALUE_MAIN: &str = r#"fun fff(s: String, n: Int = 42) = s

fun reassigned(): Int {
    var f = { -> 0 }
    val get0 = f
    f = { -> 1 }
    return run(get0) + run(f)
}

fun box(): String {
    if (reassigned() != 1) return "reassigned"
    if (new(fun(z: Int) {}) != 42) return "unread"
    if (new { } != 42) return "unread lambda"
    return "OK".app(::fff)
}
"#;

// A function value held in a local is read from that local by the body's invokes; a callable
// reference is placed like a literal lambda; a literal the body never reads leaves nothing behind.
#[test]
fn function_values_reach_a_compiled_inline_body_as_kotlinc_places_them() {
    assert_like_kotlinc(&[
        ("lib.kt", FUNCTION_VALUE_LIBRARY),
        ("main.kt", FUNCTION_VALUE_MAIN),
    ]);
}

#[test]
fn function_values_reach_a_compiled_inline_body_and_run() {
    common::expect_box_ok_files_with_stdlib(
        &[
            ("lib.kt", FUNCTION_VALUE_LIBRARY),
            ("main.kt", FUNCTION_VALUE_MAIN),
        ],
        "FunctionValues",
    );
}

// A callee with a try/catch inlined beside operands already on the stack saves them around its
// body, as kotlinc's `FixStack` does.
const TRY_CATCH: &str = r#"inline fun <T> tryOrElse(f1: () -> T, f2: () -> T): T {
    try {
        return f1()
    } catch (e: Exception) {
        return f2()
    }
}

fun testIt() = "abc" + tryOrElse({ "def" }, { "oops" }) + "ghi"

fun box(): String = if (testIt() == "abcdefghi") "OK" else testIt()
"#;

#[test]
fn a_compiled_inline_body_with_a_try_catch_saves_the_operands_on_the_stack() {
    assert_eq!(
        common::expect_box_run_with_stdlib(TRY_CATCH, "TryCatch"),
        "OK"
    );
}

// A lambda that leaves by a non-local jump keeps the call expanded in IR.
const NON_LOCAL_JUMPS: &str = r#"inline fun foo(block: () -> Int): Int = block()

inline fun g(block: () -> Unit): Unit = block()

fun sum(): Int {
    var sum = 0
    for (i in 1..10) {
        sum += foo { if (i == 3) break else i }
    }
    return sum
}

fun box(): String {
    if (sum() != 3) return "break"
    try {
        for (c in arrayOf("x")) {
            g {
                for (d in arrayOf(1)) {
                    return "OK"
                }
            }
        }
    } finally {}
    return "return"
}
"#;

#[test]
fn a_lambda_leaving_by_a_non_local_jump_runs() {
    assert_eq!(
        common::expect_box_run_with_stdlib(NON_LOCAL_JUMPS, "NonLocalJumps"),
        "OK"
    );
}

// A bound reference to a value class member, whose receiver is boxed until `!!`, captures the
// unboxed value.
// A `break` or `continue` of a range loop inside the literal stays in the literal: the call is
// inlined from the compiled body, and a library call's literal takes the MethodInliner as well.
const RANGE_LOOP_JUMPS: &str = r#"inline fun <T> measured(block: () -> T): T = block()

fun box(): String {
    val found = measured {
        var last = 0
        for (i in 0..9) {
            if (i == 3) break
            if (i == 1) continue
            last = i
        }
        last
    }
    val listed = listOf(1, 2, 3).map {
        var sum = 0
        for (i in 0 until it) {
            if (i == 2) break
            sum += i
        }
        sum
    }
    return if (found == 2 && listed == listOf(0, 1, 1)) "OK" else "fail $found $listed"
}
"#;

#[test]
fn a_local_jump_in_a_range_loop_of_an_inlined_literal_is_inlined_like_kotlinc() {
    assert_like_kotlinc(&[("main.kt", RANGE_LOOP_JUMPS)]);
}

const BOUND_VALUE_CLASS_REFERENCE: &str = r#"inline fun foo(init: () -> String): String = init()

@JvmInline
value class IcInt(val i: Int) {
    fun simple(): String = i.toString()
}

fun testBoxed(i: IcInt?): String = foo(i!!::simple)

fun box(): String = if (testBoxed(IcInt(3)) == "3") "OK" else "fail"
"#;

#[test]
fn a_bound_value_class_reference_captures_the_unboxed_receiver() {
    assert_eq!(
        common::expect_box_run_with_stdlib(BOUND_VALUE_CLASS_REFERENCE, "BoundReference"),
        "OK"
    );
}

// A reified callee is inlined from its compiled body: the body's `reifiedOperationMarker` calls name
// the type parameter, and the inliner repoints the `checkcast`/`instanceof` after each to the call's
// type argument. A smart-cast scalar the body casts to `T` is boxed before the marker, and a
// `Nothing?` argument names `java/lang/Void`.
const REIFIED_LIBRARY: &str = r#"inline fun <reified T> isOf(x: Any?): Boolean = x is T

inline fun <reified T> castTo(x: Any?): T = x as T

inline fun <reified T> negated(value: T): T {
    return when (value) {
        is Float -> -value as T
        is Double -> -value as T
        else -> throw NumberFormatException()
    }
}
"#;

const REIFIED_MAIN: &str = r#"fun box(): String {
    if (!isOf<String>("a")) return "is String"
    if (isOf<Int>("a")) return "is Int"
    if (negated(0.0).toString() != "-0.0") return "Double"
    if (negated(0.0f).toString() != "-0.0") return "Float"
    if (castTo<Nothing?>(null) != null || isOf<Nothing?>("a")) return "Nothing?"
    return castTo<String>("OK")
}
"#;

#[test]
fn a_reified_callee_is_inlined_from_the_compiled_body() {
    assert_like_kotlinc(&[("lib.kt", REIFIED_LIBRARY), ("main.kt", REIFIED_MAIN)]);
}

#[test]
fn a_reified_callee_inlined_from_the_compiled_body_runs() {
    common::expect_box_ok_files_with_stdlib(
        &[("lib.kt", REIFIED_LIBRARY), ("main.kt", REIFIED_MAIN)],
        "Reified",
    );
}

// A callee that creates nested arrays of its reified type parameter is inlined from its compiled
// body, whose `NEW_ARRAY` markers the inliner repoints at `String[]` and `String[][]`.
const REIFIED_ARRAY: &str = r#"inline fun <reified T> jagged(n: Int, value: T): Array<Array<T>> =
    Array(n) { Array(n) { value } }

inline fun <reified T> packed(vararg elements: T): Array<out T> = arrayOf(*elements)

fun box(): String {
    val grid = jagged(2, "K")
    if (grid[1][1] != "K") return "jagged"
    val array: Array<out String> = packed("O", "K")
    return array[0] + grid[0][0]
}
"#;

#[test]
fn a_callee_creating_a_reified_array_runs() {
    assert_eq!(
        common::expect_box_run_with_stdlib(REIFIED_ARRAY, "ReifiedArray"),
        "OK"
    );
}

// kotlinc marks each array a reified body creates of its parameter with `reifiedOperationMarker(0,
// …)` before the `anewarray`, naming nested arrays with a `[` per level and a nullable element of
// the created array with `?`, read before the levels are descended (an `Array<Array<T?>>` names
// `[T`, an `Array<Array<T>?>` names `[T?`); the inliner repoints it at the call-site class.
const REIFIED_ARRAY_LIBRARY: &str = r#"inline fun <reified T> two(a: T, b: T): Array<T> = arrayOf(a, b)

inline fun <reified T> nulls(n: Int): Array<T?> = arrayOfNulls<T>(n)

inline fun <reified T> filled(n: Int, x: T): Array<T> = Array<T>(n) { x }

inline fun <reified T> rows(x: T): Array<Array<T>> = arrayOf(arrayOf(x), arrayOf(x, x))

inline fun <reified T> nullableCells(x: T): Array<Array<T?>> = arrayOf(arrayOf<T?>(x, null))

inline fun <reified T> nullableRows(x: T): Array<Array<T>?> = arrayOf<Array<T>?>(arrayOf(x), null)
"#;

const REIFIED_ARRAY_MAIN: &str = r#"fun box(): String {
    val pair: Array<String> = two("O", "K")
    val empty: Array<String?> = nulls(2)
    val filled: Array<CharSequence> = filled(2, "K")
    val square: Array<Array<String>> = rows("O")
    val cells: Array<Array<String?>> = nullableCells("K")
    val sparse: Array<Array<String>?> = nullableRows("O")
    if (empty.size != 2 || filled[1] != "K") return "fail"
    if (cells[0][1] != null || sparse[1] != null || sparse[0]!![0] != "O") return "nested"
    return square[1][1] + pair[1]
}
"#;

#[test]
fn a_callee_creating_reified_arrays_is_inlined_from_the_compiled_body() {
    assert_like_kotlinc(&[
        ("lib.kt", REIFIED_ARRAY_LIBRARY),
        ("main.kt", REIFIED_ARRAY_MAIN),
    ]);
}

// A same-file callee is compiled into the caller's own class, so a private one is inlined from
// its compiled body as a public one is.
const PRIVATE_CALLEE: &str = r#"private val suffix = "K"

private inline fun <T> withSuffix(prefix: String, block: (String) -> T): T = block(prefix + suffix)

fun box(): String = withSuffix("O") { it }
"#;

#[test]
fn a_private_same_file_callee_is_inlined_from_the_compiled_body() {
    assert_like_kotlinc(&[("main.kt", PRIVATE_CALLEE)]);
}

#[test]
fn a_private_same_file_callee_inlined_from_the_compiled_body_runs() {
    assert_eq!(
        common::expect_box_run_with_stdlib(PRIVATE_CALLEE, "PrivateCallee"),
        "OK"
    );
}
