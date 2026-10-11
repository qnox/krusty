//! kotlinc's `visitWhen` marks the line of a `when` the source wrote and writes a `nop` on it
//! before the first branch, unless the `when` becomes a switch. An `if`, and a `when` the frontend
//! builds for a safe call or an elvis, gets neither.

use super::common;

fn expect_method_matches(name: &str, src: &str, class: &str, method: &str) {
    match common::class_bytes_diff_against_kotlinc(name, &[], src, class, method) {
        None => panic!("reference kotlinc is provisioned"),
        Some(Ok(())) => {}
        Some(Err(difference)) => panic!("{difference}"),
    }
}

#[test]
fn a_source_when_that_suspends_in_a_branch_matches_kotlinc() {
    let src = "suspend fun step(n: Int): Int = n\n\
suspend fun pick(x: Any): Int {\n\
    when (x) {\n\
        is String -> return step(1)\n\
        else -> return step(2)\n\
    }\n\
}\n";
    expect_method_matches(
        "SourceWhen",
        src,
        "SourceWhenKt",
        "public static final java.lang.Object pick(",
    );
}

#[test]
fn a_source_when_that_suspends_in_a_branch_resumes_there() {
    let src = "import kotlin.coroutines.*\n\
class Done : Continuation<Unit> {\n\
  override val context: CoroutineContext = EmptyCoroutineContext\n\
  override fun resumeWith(result: Result<Unit>) { result.getOrThrow() }\n\
}\n\
var parked: Continuation<Int>? = null\n\
suspend fun step(): Int = suspendCoroutine { parked = it }\n\
suspend fun pick(x: Any): Int {\n\
    when (x) {\n\
        is String -> return step() + 1\n\
        else -> return step() + 2\n\
    }\n\
}\n\
fun box(): String {\n\
    var a = -1\n\
    var b = -1\n\
    suspend { a = pick(\"s\") }.startCoroutine(Done())\n\
    parked!!.resume(10)\n\
    suspend { b = pick(0) }.startCoroutine(Done())\n\
    parked!!.resume(20)\n\
    return if (a == 11 && b == 22) \"OK\" else \"F:$a:$b\"\n\
}\n";
    common::expect_box_ok_with_stdlib(src, "SourceWhenResume");
}

#[test]
fn a_subjectless_source_when_matches_kotlinc() {
    let src = "fun sign(x: Int): Int = when {\n\
    x > 0 -> 1\n\
    x < 0 -> -1\n\
    else -> 0\n\
}\n";
    expect_method_matches(
        "SubjectlessWhen",
        src,
        "SubjectlessWhenKt",
        "public static final int sign(",
    );
}

#[test]
fn an_if_a_safe_call_and_an_elvis_get_no_when_nop() {
    let src = "class Box(val v: Int)\n\
fun generated(s: Box?, b: Boolean): Int {\n\
    val n = s?.v ?: 0\n\
    if (b) return n\n\
    return n + 1\n\
}\n";
    expect_method_matches(
        "GeneratedWhens",
        src,
        "GeneratedWhensKt",
        "public static final int generated(",
    );
}
