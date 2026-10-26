//! kotlinc's line-break rules for split membership and type-test conditions.
//!
//! `parseWhenCondition` disables newlines for each `when` condition, so a line break inside one is
//! whitespace (`x` then `!in 1..3 ->`, `!is` then `String ->`). Outside a condition a line break may
//! follow `..`, `..<`, `until`, `downTo` and `is`/`!is` (`rangeExpression`, `infixFunctionCall` and
//! `isOperator` allow `NL*` after the operator), in membership tests and `for` headers alike.

use super::common;

/// Split forms whose bytes need no other rule: no constant bound moves across the break.
const MATCHING: &str = r#"fun subjectTypeSplit(x: Any): String = when (x) {
    !is
        String -> "other"
    else -> "text"
}

fun typeTestSplit(x: Any): Boolean = x !is
    String

fun untilSplit(x: Int): Boolean = x !in 1 until
    3

fun downToSplit(x: Int): Boolean = x in 3 downTo
    1
"#;

/// Split forms that parse only under the same rules.
const PARSED: &str = r#"
fun subjectSplitRange(x: Int): String = when (x) {
    !in 1..
        3 -> "out"
    else -> "in"
}

fun noSubjectOperatorSplit(x: Int): String = when {
    x
        !in 1..3 -> "out"
    else -> "in"
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

fun openEndSplit(x: Int): Boolean = x in 1..<
    3

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
        subjectTypeSplit(1), subjectTypeSplit("s"), typeTestSplit(1).toString(),
        typeTestSplit("s").toString(), untilSplit(3).toString(), untilSplit(2).toString(),
        downToSplit(1).toString(), downToSplit(0).toString(), subjectSplitRange(4),
        subjectSplitRange(1), noSubjectOperatorSplit(9), noSubjectOperatorSplit(2), longSplit(4L),
        longSplit(3L), charSplit('g'), charSplit('f'), openEndSplit(2).toString(),
        openEndSplit(3).toString(), loopSplitEnd().toString(), loopSplitOperator().toString(),
    )
    val expected = listOf(
        "other", "text", "true", "false", "true", "false", "true", "false", "out", "in", "out",
        "in", "out", "in", "out", "hex", "true", "false", "6", "6",
    )
    return if (results == expected) "OK" else results.toString()
}
"#;

#[test]
fn split_conditions_without_a_moved_bound_are_byte_identical_to_kotlinc() {
    common::assert_classes_identical_to_kotlinc_jdk(
        "SplitLineConditions",
        MATCHING,
        &["SplitLineConditionsKt"],
    );
}

#[test]
fn split_conditions_parse_and_run_like_kotlinc() {
    common::expect_box_same_as_kotlinc(
        &format!("{MATCHING}{PARSED}{BOX}"),
        "split_line_conditions_parse",
    );
}
