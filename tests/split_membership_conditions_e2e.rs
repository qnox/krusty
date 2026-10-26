//! Membership and type-test conditions split across lines, as kotlinc 2.4.20 compiles them.
//!
//! A constant range bound kotlinc moves to its exclusive neighbour (`1..3` tested as `x < 4`), or
//! converts to the comparison type, is built at the written constant's offsets, so the bound's line
//! is marked where the moved constant is loaded and the comparison's line again at its jump. The
//! `when` condition, `!in`/`!is` and range forms written here also exercise kotlinc's line-break
//! rules: a line break after `..`, `until`, `downTo` or `is`/`!is`, and anywhere inside a `when`
//! condition, is whitespace.

use super::common;

const FUNCTIONS: &str = r#"fun subjectNegated(x: Int, y: Any): String = when (x) {
    !in 1..3 -> "out"
    else -> if (y !is String) "other" else "text"
}

fun noSubjectNegated(x: Int, y: Any): String = when {
    x !in 1..3 -> "out"
    y !is String -> "other"
    else -> "fine"
}

fun noSubjectSplitBound(x: Int, y: Any): String = when {
    x !in
        1..3 -> "out"
    y !is String ->
        "other"
    else -> "fine"
}

fun noSubjectSplitPositive(x: Int): String = when {
    x in
        1..3 -> "in"
    else -> "out"
}

fun subjectSplitBound(x: Int): String = when (x) {
    !in
        1..3 -> "out"
    else -> "in"
}

fun subjectSplitRange(x: Int): String = when (x) {
    !in 1..
        3 -> "out"
    else -> "in"
}

fun subjectTypeSplit(x: Any): String = when (x) {
    !is
        String -> "other"
    else -> "text"
}

fun noSubjectOperatorSplit(x: Int): String = when {
    x
        !in 1..3 -> "out"
    else -> "in"
}

fun typeTestSplit(x: Any): Boolean = x !is
    String

fun ifSplitBound(x: Int): String {
    if (x !in
        1..3) return "out"
    return "in"
}

fun longSplit(x: Long): String = when {
    x !in 1L..
        3L -> "out"
    else -> "in"
}

fun charSplit(c: Char): String = when (c) {
    !in 'a'..
        'f' -> "out"
    else -> "hex"
}

fun untilSplit(x: Int): Boolean = x !in 1 until
    3

fun downToSplit(x: Int): Boolean = x in 3 downTo
    1

fun loopSplitEnd(): Int {
    var s = 0
    for (i in 1..
        3) s += i
    return s
}

fun loopSplitOperator(): Int {
    var s = 0
    for (i in 1
        ..3) s += i
    return s
}
"#;

const BOX: &str = r#"
fun box(): String {
    val results = listOf(
        subjectNegated(0, "s"), subjectNegated(2, 1), subjectNegated(2, "s"),
        noSubjectNegated(5, "s"), noSubjectNegated(2, 1), noSubjectNegated(2, "s"),
        noSubjectSplitBound(4, "s"), noSubjectSplitBound(3, 1), noSubjectSplitBound(1, "s"),
        noSubjectSplitPositive(3), noSubjectSplitPositive(4),
        subjectSplitBound(0), subjectSplitBound(3), subjectSplitRange(4), subjectSplitRange(1),
        subjectTypeSplit(1), subjectTypeSplit("s"), noSubjectOperatorSplit(9),
        noSubjectOperatorSplit(2), typeTestSplit(1).toString(), typeTestSplit("s").toString(),
        ifSplitBound(4), ifSplitBound(3), longSplit(4L), longSplit(3L), charSplit('g'),
        charSplit('f'), untilSplit(3).toString(), untilSplit(2).toString(),
        downToSplit(1).toString(), downToSplit(0).toString(), loopSplitEnd().toString(),
        loopSplitOperator().toString(),
    )
    val expected = listOf(
        "out", "other", "text", "out", "other", "fine", "out", "other", "fine", "in", "out",
        "out", "in", "out", "in", "other", "text", "out", "in", "true", "false", "out", "in",
        "out", "in", "out", "hex", "true", "false", "true", "false", "6", "6",
    )
    return if (results == expected) "OK" else results.toString()
}
"#;

#[test]
fn split_membership_and_type_conditions_are_byte_identical_to_kotlinc() {
    common::assert_classes_identical_to_kotlinc_jdk(
        "SplitMembershipConditions",
        FUNCTIONS,
        &["SplitMembershipConditionsKt"],
    );
}

#[test]
fn split_membership_and_type_conditions_run_like_kotlinc() {
    common::expect_box_same_as_kotlinc(&format!("{FUNCTIONS}{BOX}"), "split_membership_conditions");
}
