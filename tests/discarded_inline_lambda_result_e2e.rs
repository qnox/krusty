//! A statement-position inline call whose lambda ends in a value discards that value after the
//! call, not inside the lambda.
//!
//! kotlinc leaves the inlined lambda's result on the operand stack through the end of the lambda's
//! locals and the call line it returns to (`markLineNumberAfterInlineIfNeeded`); the statement's
//! `pop` follows there. That `pop` is the call line's instruction, so the frame's closing `nop` is
//! redundant and the cleanup drops it. Popping inside the lambda left the `pop` on the lambda's line
//! and made the closing `nop` the call line's only instruction: one byte longer, with the lambda's
//! locals one byte wider.

use super::common;

const SOURCE: &str = "fun one(): Int = 1\n\
    fun two(): Long = 2L\n\
    fun multiLine(): String {\n\
    \x20   run {\n\
    \x20       one()\n\
    \x20   }\n\
    \x20   return \"O\"\n\
    }\n\
    fun receiver(): String {\n\
    \x20   with(\"x\") {\n\
    \x20       length\n\
    \x20   }\n\
    \x20   return \"K\"\n\
    }\n\
    fun parameter(s: String): String {\n\
    \x20   s.let {\n\
    \x20       it.length\n\
    \x20   }\n\
    \x20   return s\n\
    }\n\
    fun oneLine(): String {\n\
    \x20   run { one() }\n\
    \x20   return \"\"\n\
    }\n\
    fun wide(): String {\n\
    \x20   run { two() }\n\
    \x20   return \"\"\n\
    }\n\
    fun unitEnd() {\n\
    \x20   run {\n\
    \x20       one()\n\
    \x20   }\n\
    }\n\
    fun box(): String {\n\
    \x20   unitEnd()\n\
    \x20   return multiLine() + receiver() + parameter(\"\") + oneLine() + wide()\n\
    }\n";

#[test]
fn a_discarded_inline_lambda_result_pops_on_the_call_line_like_kotlinc() {
    let classes = common::classes_against_kotlinc_module(&[("Lambdas.kt", SOURCE)]);
    assert_eq!(classes.differences(), Vec::<String>::new());
}

#[test]
fn a_discarded_inline_lambda_result_runs() {
    common::expect_box_ok_files_with_stdlib(&[("Lambdas.kt", SOURCE)], "discarded lambda result");
}
