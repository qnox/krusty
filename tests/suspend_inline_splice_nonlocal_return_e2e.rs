//! A non-local `return` out of a lambda spliced into a CLASSPATH `inline fun` is a return from the
//! ENCLOSING suspend function, and the CPS method returns `Object`. The suspend pass boxes every
//! return it reaches so that a primitive result is legal there, but it stopped at the lambda: the
//! `return 100` the splice materializes inside the loop body came out as `bipush 100; areturn`, and
//! the class failed verification at load (`Bad type on operand stack`). A bare `return` in a
//! `Unit` suspend function has the same hole (`return` where the method expects a value).
//!
//! The dependency is krusty-built by default (the gate) and reference-built under
//! `KRUSTY_REF_KOTLINC=1`. Both shapes failed before the fix with the same two VerifyErrors; the
//! reference build is kept meaningful because the original report came from a kotlinc-built loop
//! that holds the accumulator on the operand stack when the lambda runs.

use super::common;

const LIB: &str = "inline fun twice(x: Int, f: (Int) -> Int): Int {\n\
    \x20   var acc = 0\n\
    \x20   var i = 0\n\
    \x20   while (i < 2) { acc = acc + f(x + i); i = i + 1 }\n\
    \x20   return acc\n\
    }\n";

#[test]
fn a_primitive_non_local_return_out_of_a_spliced_lambda_is_boxed() {
    let main = "suspend fun f(): Int {\n\
        \x20   twice(1) { x -> if (x == 2) return 100; x }\n\
        \x20   return -1\n\
        }\n";
    common::expect_suspend_result_against_ref(
        "suspend_inline_splice_nonlocal_return_int",
        LIB,
        main,
        "f(continuation)",
        "100",
    );
}

#[test]
fn a_bare_non_local_return_out_of_a_spliced_lambda_yields_unit() {
    let main = "suspend fun u() {\n\
        \x20   twice(1) { x -> if (x == 2) return; x }\n\
        \x20   throw IllegalStateException(\"fell through\")\n\
        }\n";
    common::expect_suspend_result_against_ref(
        "suspend_inline_splice_nonlocal_return_unit",
        LIB,
        main,
        "u(continuation)",
        "kotlin.Unit",
    );
}
