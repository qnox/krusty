//! An inlined lambda whose result is another inline call returns on that result's line again.
//!
//! kotlinc marks a lambda's implicit `return` at the end of its trailing expression once the value
//! is on the stack, and the inliner leaves a `nop` where the `return` was. After an inline call
//! kotlinc has forgotten the line in effect (`markLineNumberAfterInlineIfNeeded`), so for
//! `run { run { one() } }` that mark is written again and the `nop` is its line's only instruction:
//! the cleanup keeps it, inside the outer lambda's locals. The lambda body's `}` is not that line;
//! a trailing expression that spans lines returns on its last one.

use super::common;

const SOURCE: &str = "fun one(): Int = 1\n\
    fun add(a: Int, b: Int): Int = a + b\n\
    fun nestedValue(): Int {\n\
    \x20   val x = run { run { one() } }\n\
    \x20   return x\n\
    }\n\
    fun nestedStatement(): String {\n\
    \x20   run {\n\
    \x20       run { one() }\n\
    \x20   }\n\
    \x20   return \"O\"\n\
    }\n\
    fun nestedUnitEnd() {\n\
    \x20   run { run { one() } }\n\
    }\n\
    fun innerMultiLine(): Int {\n\
    \x20   val x = run {\n\
    \x20       run {\n\
    \x20           one()\n\
    \x20       }\n\
    \x20   }\n\
    \x20   return x\n\
    }\n\
    fun receivers(s: String): Int {\n\
    \x20   val n = s.let { it.run { length } }\n\
    \x20   return n\n\
    }\n\
    fun spans(): Int {\n\
    \x20   val x = run {\n\
    \x20       add(1,\n\
    \x20           2)\n\
    \x20   }\n\
    \x20   return x\n\
    }\n\
    fun box(): String {\n\
    \x20   nestedUnitEnd()\n\
    \x20   val sum = nestedValue() + innerMultiLine() + receivers(\"ab\") + spans()\n\
    \x20   return if (sum == 7) nestedStatement() + \"K\" else \"$sum\"\n\
    }\n";

#[test]
fn a_lambda_returning_an_inline_call_marks_its_return_line_like_kotlinc() {
    let classes = common::classes_against_kotlinc_module(&[("Nested.kt", SOURCE)]);
    assert_eq!(classes.differences(), Vec::<String>::new());
}

#[test]
fn a_lambda_returning_an_inline_call_runs() {
    common::expect_box_ok_files_with_stdlib(&[("Nested.kt", SOURCE)], "nested inline lambda");
}
