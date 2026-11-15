//! An inline call in a default argument is expanded in the `$default` stub the way it is expanded
//! in any other body: the expansion opens kotlinc's inline frame, whose `$i$f$` marker store and
//! inlined locals appear in the stub's variable table. Top-level, member, primary-constructor and
//! secondary-constructor stubs share that frame, for a same-file callee, another file's callee and a
//! library callee with a lambda.

use super::common;

const SAME_FILE: &str = r#"
inline fun twice(x: Int): Int {
    val doubled = x * 2
    return doubled
}

fun top(a: Int = twice(3)) = a

class Member {
    fun member(a: Int = twice(4)) = a
}

class Primary(val a: Int = twice(5))

class Secondary(val a: Int) {
    constructor(b: String, c: Int = twice(6)) : this(c + b.length)
}

fun box(): String {
    if (top() != 6) return "top ${top()}"
    if (Member().member() != 8) return "member"
    if (Primary().a != 10) return "primary"
    if (Secondary("x").a != 13) return "secondary"
    return "OK"
}
"#;

const CALLEE: &str = r#"
inline fun offset(x: Int, by: Int): Int {
    val sum = x + by
    return sum
}
"#;

const CALLER: &str = r#"
fun shifted(a: Int = offset(10, 1), b: Int = offset(a, 5)) = a * 100 + b

class Shifted(val a: Int = offset(20, 2)) {
    fun plus(b: Int = offset(a, 3)) = a + b
}

fun box(): String {
    val value = shifted()
    if (value != 1116) return "shifted $value"
    val member = Shifted().plus()
    return if (member == 47) "OK" else "member $member"
}
"#;

const LIBRARY_LAMBDA: &str = r#"
fun counted(values: List<Int> = listOf(1, 2, 3), total: Int = values.sumOf { it * 2 }) = total

class Holder(val label: String = listOf("a", "b").first { it != "a" })

class Scaler {
    fun scaled(values: List<Int> = listOf(4, 5), total: Int = values.sumOf { it * 3 }) = total
}

fun box(): String {
    if (counted() != 12) return "counted ${counted()}"
    if (Holder().label != "b") return "holder ${Holder().label}"
    if (Scaler().scaled() != 27) return "scaled ${Scaler().scaled()}"
    return "OK"
}
"#;

fn assert_like_kotlinc(sources: &[(&str, &str)]) {
    let classes = common::classes_against_kotlinc_module(sources);
    let differences = classes.differences();
    assert!(differences.is_empty(), "{}", differences.join("\n\n"));
}

fn assert_runs_like_kotlinc(sources: &[(&str, &str)], main_class: &str) {
    assert_eq!(
        common::kotlinc_box_files_result(sources, main_class),
        "OK",
        "kotlinc's {main_class}"
    );
    common::expect_box_ok_files_with_stdlib(sources, main_class);
}

#[test]
fn a_same_file_inline_call_in_each_kind_of_default_stub_opens_kotlincs_frame() {
    assert_like_kotlinc(&[("Same.kt", SAME_FILE)]);
}

#[test]
fn a_same_file_inline_call_in_each_kind_of_default_stub_runs_like_kotlinc() {
    assert_runs_like_kotlinc(&[("Same.kt", SAME_FILE)], "SameKt");
}

#[test]
fn another_files_inline_call_in_a_default_stub_opens_kotlincs_frame() {
    assert_like_kotlinc(&[("Callee.kt", CALLEE), ("Caller.kt", CALLER)]);
}

#[test]
fn another_files_inline_call_in_a_default_stub_runs_like_kotlinc() {
    assert_runs_like_kotlinc(&[("Callee.kt", CALLEE), ("Caller.kt", CALLER)], "CallerKt");
}

#[test]
fn a_library_inline_call_with_a_lambda_in_a_default_stub_opens_kotlincs_frame() {
    assert_like_kotlinc(&[("Library.kt", LIBRARY_LAMBDA)]);
}

#[test]
fn a_library_inline_call_with_a_lambda_in_a_default_stub_runs_like_kotlinc() {
    assert_runs_like_kotlinc(&[("Library.kt", LIBRARY_LAMBDA)], "LibraryKt");
}
