//! kotlinc reads a lone `\r` as a line break, as it reads `\r\n`. A line comment ends there and a
//! statement is separated there.

use super::common;

/// Every `\n` of this program is a lone `\r`, so only the line-break rule decides whether it
/// compiles: the comments end at it and `val` statements without `;` are told apart by it.
const PROGRAM: &str = "// leading comment\rfun box(): String {\r    val a = 1 // trailing comment\r    val b = 2\r    val text = \"\"\"raw\rtext\"\"\"\r    return if (a + b == 3 && text.length == 8) \"OK\" else \"fail\"\r}\r";

#[test]
fn a_lone_carriage_return_is_a_line_break_as_in_kotlinc() {
    common::expect_box_same_as_kotlinc(PROGRAM, "LoneCarriageReturnLineBreaks");
}
