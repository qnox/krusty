//! A trailing lambda after NAMED arguments — `f(b = 2, a = 1) { … }` — is valid Kotlin (the lambda
//! fills the last parameter), but krusty rejected the lambda as "a positional argument cannot follow a
//! named argument" in both the checker (`map_call_args`) and the lowering (`lower_args_defaulted`).
//! Now the trailing lambda fills the last slot. Round-tripped on a real JVM, including a reordered call.

use super::common;

#[test]
fn trailing_lambda_after_named_args_runs() {
    // Out-of-order named args (`mod` first), a defaulted param SUPPLIED, then a trailing lambda.
    const SRC: &str =
        "fun host(ctl: Int, start: String, mod: Int = 0, builder: () -> Unit): Int {\n\
        \x20 builder(); return ctl + mod\n\
        }\n\
        fun box(): String {\n\
        \x20 var x = 0\n\
        \x20 val r = host(mod = 5, ctl = 7, start = \"a\") { x = 1 }\n\
        \x20 return if (x == 1 && r == 12) \"OK\" else \"FAIL r=$r x=$x\"\n\
        }\n";
    common::assert_box_ok_with_stdlib(SRC, "H");
}
