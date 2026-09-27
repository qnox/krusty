//! A suspension point inside a `try` goes through kotlinc's transformer like any other: the
//! handler ranges split around each suspension and the resumed state re-enters the protected
//! region.

use super::common;

fn expect_method_matches(name: &str, src: &str, class: &str, method: &str) {
    match common::method_code_diff_against_kotlinc(name, &[], src, class, method) {
        None => panic!("reference kotlinc is provisioned"),
        Some(Ok(())) => {}
        Some(Err(difference)) => panic!("{difference}"),
    }
}

const SRC: &str = "suspend fun step(): Int = 1\n\
suspend fun caught(): Int {\n\
    try {\n\
        return step() + 1\n\
    } catch (e: Throwable) {\n\
        return 2\n\
    }\n\
}\n\
suspend fun finished(): Int {\n\
    var r = 0\n\
    try {\n\
        r = step()\n\
    } finally {\n\
        r += 10\n\
    }\n\
    return r\n\
}\n\
suspend fun inCatch(): Int {\n\
    try {\n\
        return 1\n\
    } catch (e: Throwable) {\n\
        return step() + 2\n\
    }\n\
}\n\
suspend fun inFinally(): Int {\n\
    var r = 0\n\
    try {\n\
        r = 1\n\
    } finally {\n\
        r += step()\n\
    }\n\
    return r\n\
}\n\
suspend fun twoInOne(): Int {\n\
    try {\n\
        val a = step()\n\
        return a + step()\n\
    } catch (e: Throwable) {\n\
        return 0\n\
    }\n\
}\n\
suspend fun nested(): Int {\n\
    try {\n\
        try {\n\
            return step()\n\
        } finally {\n\
            step()\n\
        }\n\
    } catch (e: Throwable) {\n\
        return step() + 3\n\
    }\n\
}\n";

#[test]
fn a_suspension_under_try_catch_matches_kotlinc() {
    expect_method_matches(
        "SuspendUnderTry",
        SRC,
        "SuspendUnderTryKt",
        "public static final java.lang.Object caught(",
    );
}

#[test]
fn a_suspension_under_try_finally_matches_kotlinc() {
    expect_method_matches(
        "SuspendUnderTry",
        SRC,
        "SuspendUnderTryKt",
        "public static final java.lang.Object finished(",
    );
}

#[test]
fn suspensions_in_catch_finally_and_nested_ranges_match_kotlinc() {
    for function in ["inCatch", "inFinally", "twoInOne", "nested"] {
        expect_method_matches(
            "SuspendUnderTry",
            SRC,
            "SuspendUnderTryKt",
            &format!("public static final java.lang.Object {function}("),
        );
    }
}

#[test]
fn a_function_suspending_under_try_keeps_kotlinc_s_continuation_class() {
    for function in [
        "caught",
        "finished",
        "inCatch",
        "inFinally",
        "twoInOne",
        "nested",
    ] {
        common::byte_diff_against_kotlinc_cp(
            "SuspendUnderTry",
            SRC,
            &format!("SuspendUnderTryKt${function}$1"),
            &[common::stdlib_jar()],
        )
        .expect("reference kotlinc is provisioned")
        .expect("the continuation class is byte-identical to kotlinc's");
    }
}

#[test]
fn a_suspend_lambda_with_points_in_try_catch_and_finally_matches_kotlinc() {
    let src = "suspend fun step(): Int = 1\n\
fun make(): suspend () -> Int = {\n\
    try {\n\
        step()\n\
    } catch (e: Throwable) {\n\
        step() + 1\n\
    } finally {\n\
        step()\n\
    }\n\
}\n";
    expect_method_matches(
        "SuspendLambdaUnderTry",
        src,
        "SuspendLambdaUnderTryKt$make$1",
        "public final java.lang.Object invokeSuspend(",
    );
}

// The handler still covers the code after the resume: an exception thrown there is caught, and
// the `finally` runs once on each path.
#[test]
fn a_suspension_under_try_resumes_inside_the_protected_region() {
    let src = "import kotlin.coroutines.*\n\
class Done : Continuation<Unit> {\n\
  override val context: CoroutineContext = EmptyCoroutineContext\n\
  override fun resumeWith(result: Result<Unit>) { result.getOrThrow() }\n\
}\n\
var parked: Continuation<Int>? = null\n\
var log = \"\"\n\
suspend fun step(): Int = suspendCoroutine { parked = it }\n\
suspend fun guarded(fail: Boolean): Int {\n\
    try {\n\
        val v = step()\n\
        if (fail) throw IllegalStateException()\n\
        return v\n\
    } catch (e: IllegalStateException) {\n\
        log += \"c\"\n\
        return -1\n\
    } finally {\n\
        log += \"f\"\n\
    }\n\
}\n\
fun box(): String {\n\
    var a = 0\n\
    var b = 0\n\
    suspend { a = guarded(false) }.startCoroutine(Done())\n\
    parked!!.resume(7)\n\
    suspend { b = guarded(true) }.startCoroutine(Done())\n\
    parked!!.resume(8)\n\
    return if (a == 7 && b == -1 && log == \"fcf\") \"OK\" else \"F:$a:$b:$log\"\n\
}\n";
    common::expect_box_ok_with_stdlib(src, "SuspendUnderTryResume");
}

// Each point really suspends: a `catch` body and a `finally` body resume where they left off, the
// `finally` body runs once on a normal and on an exceptional exit, and two points in one protected
// range and points in nested ranges each resume inside their handlers.
const RESUMES: &str = "import kotlin.coroutines.*\n\
class Done : Continuation<Unit> {\n\
  override val context: CoroutineContext = EmptyCoroutineContext\n\
  override fun resumeWith(result: Result<Unit>) { result.getOrThrow() }\n\
}\n\
var parked: Continuation<Int>? = null\n\
var log = \"\"\n\
suspend fun step(): Int = suspendCoroutine { parked = it }\n\
suspend fun inCatch(fail: Boolean): Int {\n\
    try {\n\
        if (fail) throw IllegalStateException()\n\
        return 1\n\
    } catch (e: IllegalStateException) {\n\
        log += \"c\"\n\
        return step() + 2\n\
    }\n\
}\n\
suspend fun inFinally(fail: Boolean): Int {\n\
    var r = 0\n\
    try {\n\
        if (fail) throw IllegalStateException()\n\
        r = 1\n\
    } finally {\n\
        log += \"f\"\n\
        r += step()\n\
        log += \"g\"\n\
    }\n\
    return r\n\
}\n\
suspend fun twoInOne(fail: Boolean): Int {\n\
    try {\n\
        val a = step()\n\
        val b = step()\n\
        if (fail) throw IllegalStateException()\n\
        return a + b\n\
    } catch (e: IllegalStateException) {\n\
        log += \"t\"\n\
        return 0\n\
    }\n\
}\n\
suspend fun nested(fail: Boolean): Int {\n\
    try {\n\
        try {\n\
            val v = step()\n\
            if (fail) throw IllegalStateException()\n\
            return v\n\
        } finally {\n\
            log += \"i\"\n\
            step()\n\
            log += \"j\"\n\
        }\n\
    } catch (e: IllegalStateException) {\n\
        log += \"o\"\n\
        return step() + 3\n\
    }\n\
}\n\
fun start(block: suspend () -> Unit) { block.startCoroutine(Done()) }\n\
fun resume(value: Int) { parked!!.resume(value) }\n\
fun box(): String {\n\
    var a = 0; var b = 0; var c = 0; var d = 0; var e = -1; var f = 0; var g = 0\n\
    start { a = inCatch(false) }\n\
    start { b = inCatch(true) }\n\
    resume(5)\n\
    start { c = inFinally(false) }\n\
    resume(4)\n\
    start { try { inFinally(true) } catch (x: IllegalStateException) { log += \"x\" } }\n\
    resume(9)\n\
    start { d = twoInOne(false) }\n\
    resume(2)\n\
    resume(3)\n\
    start { e = twoInOne(true) }\n\
    resume(2)\n\
    resume(3)\n\
    start { f = nested(false) }\n\
    resume(6)\n\
    resume(0)\n\
    start { g = nested(true) }\n\
    resume(6)\n\
    resume(0)\n\
    resume(7)\n\
    val r = \"$a:$b:$c:$d:$e:$f:$g:$log\"\n\
    return if (r == \"1:7:5:5:0:6:10:cfgfgxtijijo\") \"OK\" else \"F:$r\"\n\
}\n";

#[test]
fn suspensions_in_catch_finally_and_nested_ranges_resume_once() {
    common::expect_box_ok_with_stdlib(RESUMES, "SuspendUnderTryRanges");
}
