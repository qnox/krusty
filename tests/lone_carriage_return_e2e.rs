//! kotlinc reads a lone `\r` as a line break, as it reads `\r\n`. A line comment ends there, a
//! statement is separated there, a raw string holds it as `\n`, and it starts a new line for the
//! debug line numbers.

use super::common;

/// Every line break of this program is a lone `\r`, so only the line-break rule decides whether it
/// compiles: the comments end at it and `val` statements without `;` are told apart by it.
const PROGRAM: &str = include_str!("box_regressions/loneCarriageReturnLineBreaks.kt");

#[test]
fn a_lone_carriage_return_is_a_line_break_as_in_kotlinc() {
    common::expect_box_same_as_kotlinc(PROGRAM, "LoneCarriageReturnLineBreaks");
}

// PLATFORM-SPECIFIC: JVM stack trace line numbers (`LineNumberTable`).
#[test]
fn a_lone_carriage_return_starts_a_new_debug_line_as_in_kotlinc() {
    common::expect_box_same_as_kotlinc(
        "fun box(): String {\r    val line = Throwable().stackTrace[0].lineNumber\r    return if (line == 2) \"OK\" else \"line=$line\"\r}\r",
        "LoneCarriageReturnDebugLines",
    );
}
