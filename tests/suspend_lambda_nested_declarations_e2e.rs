//! What a suspend lambda's body declares is lifted into the lambda's own class, as kotlinc lifts
//! it out of the `invokeSuspend` the body became.
//!
//! A lambda the body builds is a private static `invokeSuspend$lambda$N` of the class, numbered
//! afresh in the class, and a local function is `invokeSuspend$<name>`. They follow the members the
//! class was declared with (its constructor, `invokeSuspend`, `create` and the typed `invoke`) and
//! precede the erased bridge, in kotlinc's lifting order: local functions first, then lambdas,
//! each nested lambda before the lambda enclosing it. A lambda an inline call splices builds no
//! closure and stays in the body, and an inline suspend callee reads the lambda's continuation,
//! `this` cast to `Continuation`, where it loads it.

use super::common;

#[test]
fn sibling_lambdas_are_numbered_within_the_class() {
    let src = "fun keep(a: Any) {}\n\
fun noinlineRun(block: () -> Unit) { block() }\n\
suspend fun leaf(): Int = 1\n\
fun nested(): suspend () -> Unit = { noinlineRun { keep(1) }; leaf(); noinlineRun { keep(2) } }\n";
    common::assert_classes_identical_to_kotlinc("SlnSiblings", src, &["SlnSiblingsKt$nested$1"]);
}

#[test]
fn a_nested_lambda_precedes_the_lambda_enclosing_it() {
    let src = "fun keep(a: Any) {}\n\
fun noinlineRun(block: () -> Unit) { block() }\n\
suspend fun leaf(): Int = 1\n\
fun deep(): suspend () -> Unit = { noinlineRun { noinlineRun { keep(3) } }; leaf() }\n";
    common::assert_classes_identical_to_kotlinc("SlnDeep", src, &["SlnDeepKt$deep$1"]);
}

#[test]
fn a_lambda_capturing_a_parameter_and_a_capture_is_lifted_with_them() {
    let src = "fun keep(a: Any) {}\n\
fun noinlineRun(block: () -> Unit) { block() }\n\
suspend fun leaf(): Int = 1\n\
fun nestedCap(x: String): suspend (Int) -> Unit = { n -> leaf(); noinlineRun { keep(x + n) } }\n";
    common::assert_classes_identical_to_kotlinc("SlnCapture", src, &["SlnCaptureKt$nestedCap$1"]);
}

#[test]
fn a_local_function_becomes_a_static_of_the_class() {
    let src = "fun keep(a: Any) {}\n\
suspend fun leaf(): Int = 1\n\
fun declaresLocal(): suspend () -> Unit = {\n\
    fun twice(x: Int) = x * 2\n\
    keep(twice(leaf()))\n\
}\n";
    common::assert_classes_identical_to_kotlinc(
        "SlnLocal",
        src,
        &["SlnLocalKt", "SlnLocalKt$declaresLocal$1"],
    );
}

#[test]
fn a_nested_suspend_lambda_is_a_class_of_its_own_inside_the_class() {
    let src = "fun keep(a: Any) {}\n\
fun noinlineRun(block: () -> Unit) { block() }\n\
suspend fun leaf(): Int = 1\n\
fun builder(c: suspend () -> Unit) {}\n\
fun twoLevels(): suspend () -> Unit = { builder { noinlineRun { keep(4) }; leaf() }; leaf() }\n";
    common::assert_classes_identical_to_kotlinc(
        "SlnTwoLevels",
        src,
        &["SlnTwoLevelsKt$twoLevels$1", "SlnTwoLevelsKt$twoLevels$1$1"],
    );
}

#[test]
fn an_inline_suspend_callee_reads_the_lambda_as_its_continuation() {
    // `suspendCoroutine`'s lambda is spliced into `invokeSuspend`; its continuation is not stored
    // ahead of the state's `label`, so nothing is spilled for it.
    let src = "import kotlin.coroutines.*\n\
class Holder { var c: Continuation<Int>? = null }\n\
val holder = Holder()\n\
fun suspended(): suspend () -> Int = { suspendCoroutine { holder.c = it } }\n";
    common::assert_classes_identical_to_kotlinc("SlnInline", src, &["SlnInlineKt$suspended$1"]);
}

/// A dependency's inline suspend callee with a default parameter. Being reified, it has no callable
/// `$default` to fall back to: the call must splice it.
const AWAITING: &str = r#"package awaiting

import kotlin.coroutines.*

var parked: Continuation<Int>? = null

suspend inline fun <reified T> await(tag: String = "t"): Int =
    suspendCoroutine { parked = it; tag.length + (T::class.java.hashCode() and 0) }
"#;

#[test]
fn a_defaulted_inline_suspend_callee_reads_the_lambda_as_its_continuation() {
    // An omitted default argument puts the continuation before the `$default` mask and marker,
    // not last; the spliced callee still reads the lambda where it loads its continuation, so
    // nothing but the default value is spilled. kotlinc also folds the constant mask out of the
    // spliced `$default` prologue, which krusty does not yet, so the member tables are compared
    // rather than the bytes.
    let src = "import awaiting.*\n\
fun suspended(): suspend () -> Int = { await<String>() }\n";
    let classes =
        common::classes_against_kotlinc_lib("SlnDefaultInline", &[("Lib.kt", AWAITING)], src)
            .expect("reference toolchain is provisioned");
    let class = "SlnDefaultInlineKt$suspended$1";
    assert_eq!(
        common::member_table(&classes.krusty[class]),
        common::member_table(&classes.reference[class])
    );
}

#[test]
fn lifted_declarations_run_from_the_class() {
    let src = "import kotlin.coroutines.*\n\
fun noinlineRun(block: () -> Unit) { block() }\n\
var log = \"\"\n\
var saved: Continuation<Int>? = null\n\
fun builder(c: suspend () -> Unit) { c.startCoroutine(Continuation(EmptyCoroutineContext) { it.getOrThrow() }) }\n\
fun box(): String {\n\
    builder {\n\
        fun twice(x: Int) = x * 2\n\
        noinlineRun { log += \"a\" }\n\
        val n = suspendCoroutine<Int> { saved = it }\n\
        noinlineRun { noinlineRun { log += twice(n) } }\n\
    }\n\
    saved!!.resume(21)\n\
    return if (log == \"a42\") \"OK\" else \"F:$log\"\n\
}\n";
    common::expect_box_ok_with_stdlib(src, "SlnRun");
}
