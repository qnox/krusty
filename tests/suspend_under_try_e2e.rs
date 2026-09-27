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
