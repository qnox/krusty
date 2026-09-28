//! Top-level extension functions `fun Recv.name(…)` — compiled as static methods whose first
//! parameter is the receiver (Kotlin's strategy). Same-named extensions on different receivers don't
//! collide (dispatched by receiver). A user `operator fun` extension overrides the builtin operator.
//! Round-tripped under `-Xverify:all`.

use super::common;

#[test]
fn extension_functions_run() {
    let src = "fun Int.dbl(): Int = this * 2\n\
fun String.dbl(): String = this + this\n\
fun Int.plusX(x: Int): Int = this + x\n\
fun box(): String {\n\
if (3.dbl() != 6) return \"f1\"\n\
if (\"a\".dbl() != \"aa\") return \"f2\"\n\
if (3.plusX(4) != 7) return \"f3\"\n\
return \"OK\"\n\
}\n";
    common::expect_box_ok_with_stdlib(src, "D");
}

/// A parenthesized extension callee is evaluated before its receiver.
/// Official box `extensionFunctions/executionOrder.kt`.
#[test]
fn parenthesized_extension_callee_runs_before_the_receiver() {
    let src = "var result = \"\"\n\
fun getReceiver(): Int {\n\
result += \"getReceiver->\"\n\
return 1\n\
}\n\
fun getFun(b: Int.(Int) -> Unit): Int.(Int) -> Unit {\n\
result += \"getFun()->\"\n\
return b\n\
}\n\
fun box(): String {\n\
getReceiver().(getFun({ result += \"End\" }))(1)\n\
return if (result == \"getFun()->getReceiver->End\") \"OK\" else result\n\
}\n";
    common::expect_box_ok_with_stdlib(src, "executionOrder");
}
