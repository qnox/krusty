//! End-to-end `box()` coverage for `suspend`/coroutine lowering (`src/jvm/suspend.rs`,
//! `src/resolve.rs`), complementary to `tests/feature_coverage_j_e2e.rs`.
//!
//! Each test compiles a self-contained Kotlin program whose `fun box(): String` drives one or more
//! `suspend` functions through a local `runBlocking` (a synchronous coroutine driver built on
//! `startCoroutine`, mirroring the box-corpus `CoroutineUtil.kt`) and returns "OK". The program is
//! compiled in-process and its `box()` runs on the persistent JVM via `common::compile_and_run_box`.
//! All suspend callees complete synchronously, so the whole state machine runs to completion under
//! bytecode verification — proving krusty's CPS lowering, spilling and resumption are byte-correct.
//!
//! These are DIFFERENT scenarios from `tests/feature_coverage_j_e2e.rs` and `tests/suspend_e2e.rs`:
//! here we stress varied state-machine SHAPES — deep nesting, many spilled locals, loops with several
//! suspension points, suspend lambda parameters invoked in loops, generic/mutually-recursive suspend
//! funs, `try`/`finally` around suspension, and suspend extension functions.

use super::common;

/// Coroutine driver + intrinsics, prepended to every program. A synchronous `runBlocking` built on
/// `startCoroutine`, exactly like the box corpus injects for `// WITH_COROUTINES` tests.
const PREAMBLE: &str = r#"import kotlin.coroutines.*
import kotlin.coroutines.intrinsics.*

fun <T> runBlocking(block: suspend () -> T): T {
    var res: Result<T>? = null
    block.startCoroutine(Continuation(EmptyCoroutineContext) {
        res = it
    })
    return res!!.getOrThrow()
}
"#;

/// Compile `PREAMBLE + body` as one file (stem `stem`) and run its `box()` on the shared JVM.
/// Returns the trimmed `box()` output, or `None` when the Kotlin/JDK toolchain is unavailable
/// (the caller then skips rather than fails spuriously).
fn run_box(stem: &str, body: &str) -> Option<String> {
    let stdlib = common::stdlib_jar()?;
    let jdk = common::jdk_modules();
    let src = format!("{PREAMBLE}{body}");
    common::compile_and_run_box(&src, stem, &[stdlib], jdk.as_deref()).map(|s| s.trim().to_string())
}

#[test]
fn suspend_returns_data_class() {
    // A suspend fun returning a user `data class`; the reference value flows through the Object-erased
    // CPS return and is destructured after the suspension point.
    let body = r#"
data class Point(val x: Int, val y: Int)
suspend fun locate(): Point = Point(3, 4)
suspend fun compute(): Int {
    val p = locate()
    return p.x + p.y
}
fun box(): String {
    val r = runBlocking { compute() }
    return if (r == 7) "OK" else "FAIL:$r"
}
"#;
    if let Some(out) = run_box("SuspendDataClass", body) {
        assert_eq!(out, "OK", "suspend returning data class");
    }
}

// DROPPED: `suspend fun` returning a nullable `Int?` consumed by an elvis after the suspension
// point. krusty miscompiles the spill of the nullable-boxed local across the suspension: the state
// machine's stackmap frame types the reloaded slot as `java/lang/Integer` while the resume path
// pushes `java/lang/Object`, yielding a `VerifyError` (inconsistent stackmap at the elvis branch
// target). This is a compiler bug in `src/jvm/suspend.rs`; per task rules the scenario is dropped
// rather than worked around, and the compiler is left unmodified.

#[test]
fn suspend_returns_value_class() {
    // A suspend fun returning a `@JvmInline value class`; the boxed representation crosses the CPS
    // boundary (an inline-class value at a generic boundary must be boxed, then unwrapped on use).
    let body = r#"
@JvmInline
value class Meters(val v: Int)
suspend fun distance(): Meters = Meters(42)
suspend fun compute(): Int {
    val m = distance()
    return m.v
}
fun box(): String {
    val r = runBlocking { compute() }
    return if (r == 42) "OK" else "FAIL:$r"
}
"#;
    if let Some(out) = run_box("SuspendValueClass", body) {
        assert_eq!(out, "OK", "suspend returning value class");
    }
}

#[test]
fn suspend_when_returns_from_multiple_arms() {
    // A `when` whose arms each `return` the result of a distinct suspend call — several suspension
    // points, only the selected arm runs.
    let body = r#"
suspend fun a(): Int = 10
suspend fun b(): Int = 20
suspend fun c(): Int = 30
suspend fun pick(n: Int): Int {
    when (n) {
        0 -> return a()
        1 -> return b()
        else -> return c()
    }
}
fun box(): String {
    val r = runBlocking { pick(0) } + runBlocking { pick(1) } + runBlocking { pick(2) }
    return if (r == 60) "OK" else "FAIL:$r"
}
"#;
    if let Some(out) = run_box("SuspendWhenReturns", body) {
        assert_eq!(out, "OK", "when returning from multiple suspend arms");
    }
}

#[test]
fn deeply_nested_suspend_calls() {
    // Six suspend functions calling each other in a chain — deep CPS threading, each frame resuming
    // into the next.
    let body = r#"
suspend fun l6(): Int = 1
suspend fun l5(): Int = l6() + 1
suspend fun l4(): Int = l5() + 1
suspend fun l3(): Int = l4() + 1
suspend fun l2(): Int = l3() + 1
suspend fun l1(): Int = l2() + 1
fun box(): String {
    val r = runBlocking { l1() }
    return if (r == 6) "OK" else "FAIL:$r"
}
"#;
    if let Some(out) = run_box("DeeplyNestedSuspend", body) {
        assert_eq!(out, "OK", "deeply nested suspend calls");
    }
}

#[test]
fn many_locals_spilled_across_suspensions() {
    // Six locals, each bound by a distinct suspend call and all read at the end — every earlier local
    // must be spilled into the continuation and reloaded as the state machine advances through six
    // suspension points.
    let body = r#"
suspend fun v(n: Int): Int = n
suspend fun compute(): Int {
    val a = v(1)
    val b = v(2)
    val c = v(3)
    val d = v(4)
    val e = v(5)
    val f = v(6)
    return a + b + c + d + e + f
}
fun box(): String {
    val r = runBlocking { compute() }
    return if (r == 21) "OK" else "FAIL:$r"
}
"#;
    if let Some(out) = run_box("ManyLocalsSpilled", body) {
        assert_eq!(out, "OK", "many locals spilled across suspensions");
    }
}

#[test]
fn for_and_while_loops_each_with_multiple_suspend_calls() {
    // A `for` loop and a `while` loop in one suspend fun, each body containing TWO suspend calls; the
    // accumulator and counters are spilled across every suspension point of both loops.
    let body = r#"
suspend fun inc(x: Int): Int = x + 1
suspend fun dbl(x: Int): Int = x * 2
suspend fun compute(): Int {
    var acc = 0
    for (i in 1..3) {
        acc = inc(acc)
        acc = dbl(acc)
    }
    var j = 0
    while (j < 2) {
        acc = inc(acc)
        acc = inc(acc)
        j = j + 1
    }
    return acc
}
fun box(): String {
    val r = runBlocking { compute() }
    // for: 0->2->6->14; while: +4 -> 18
    return if (r == 18) "OK" else "FAIL:$r"
}
"#;
    if let Some(out) = run_box("SuspendTwoLoops", body) {
        assert_eq!(out, "OK", "for+while loops with multiple suspend calls");
    }
}

#[test]
fn suspend_lambda_param_invoked_in_loop() {
    // A suspend fun taking a `suspend (Int) -> Int` parameter and invoking it repeatedly in a loop;
    // the lambda suspends on each call, the accumulator spills across iterations.
    let body = r#"
suspend fun add(a: Int, b: Int): Int = a + b
suspend fun fold(n: Int, block: suspend (Int) -> Int): Int {
    var acc = 0
    for (i in 1..n) {
        acc = block(acc)
    }
    return acc
}
fun box(): String {
    val r = runBlocking { fold(4) { acc -> add(acc, 3) } }
    return if (r == 12) "OK" else "FAIL:$r"
}
"#;
    if let Some(out) = run_box("SuspendLambdaLoop", body) {
        assert_eq!(out, "OK", "suspend lambda param invoked in loop");
    }
}

#[test]
fn suspend_calls_generic_suspend_fun() {
    // A suspend fun calling a GENERIC suspend fun `<T> identity(t: T): T` at two different type
    // arguments — the type parameter is erased through the CPS boundary.
    let body = r#"
suspend fun <T> identity(t: T): T = t
suspend fun compute(): String {
    val n = identity(41)
    val s = identity("x")
    return "" + (n + 1) + s
}
fun box(): String {
    val r = runBlocking { compute() }
    return if (r == "42x") "OK" else "FAIL:$r"
}
"#;
    if let Some(out) = run_box("SuspendGeneric", body) {
        assert_eq!(out, "OK", "suspend calling generic suspend fun");
    }
}

#[test]
fn try_finally_around_suspension() {
    // A `try`/`finally` around a suspend call — the `finally` block must run after the state machine
    // resumes past the suspension point (the exception table survives the split).
    let body = r#"
suspend fun work(): Int = 5
suspend fun compute(): Int {
    var log = 0
    try {
        val v = work()
        log = log + v
    } finally {
        log = log + 100
    }
    return log
}
fun box(): String {
    val r = runBlocking { compute() }
    return if (r == 105) "OK" else "FAIL:$r"
}
"#;
    if let Some(out) = run_box("SuspendTryFinally", body) {
        assert_eq!(out, "OK", "try/finally around suspension");
    }
}

#[test]
fn mutual_recursion_between_suspend_funs() {
    // Two mutually-recursive suspend funs (bounded): `even`/`odd` decrementing to zero. Each recursive
    // call is a suspension point, so the recursion depth stresses continuation chaining.
    let body = r#"
suspend fun isEven(n: Int): Boolean {
    if (n == 0) return true
    return isOdd(n - 1)
}
suspend fun isOdd(n: Int): Boolean {
    if (n == 0) return false
    return isEven(n - 1)
}
fun box(): String {
    val a = runBlocking { isEven(6) }
    val b = runBlocking { isOdd(6) }
    return if (a && !b) "OK" else "FAIL:$a,$b"
}
"#;
    if let Some(out) = run_box("SuspendMutualRecursion", body) {
        assert_eq!(out, "OK", "mutual recursion between suspend funs");
    }
}

#[test]
fn tail_forward_with_early_returns_boxes_them() {
    // A tail-call-forwarded suspend fn (no state machine) whose body ALSO has early returns: the CPS
    // method returns `Object`, so the early primitive return must box (`Boolean.valueOf`) and a bare
    // `return` in a `Unit` fn must yield `Unit.INSTANCE` — only the forwarded tail stays verbatim.
    // Regression: the forward path skipped return boxing entirely (`iconst_1; areturn` VerifyError).
    let body = r#"
var log = ""
suspend fun note(s: String) { log += s }
suspend fun record(s: String?) {
    s ?: return
    note(s)
}
suspend fun classify(n: Int): String {
    if (n < 0) return "neg"
    return pick(n)
}
suspend fun pick(n: Int): String = if (n == 0) "zero" else "pos"
fun box(): String {
    runBlocking { record(null) }
    runBlocking { record("x") }
    if (log != "x") return "FAIL log: $log"
    val a = runBlocking { classify(-1) }
    val b = runBlocking { classify(0) }
    val c = runBlocking { classify(5) }
    return if (a == "neg" && b == "zero" && c == "pos") "OK" else "FAIL:$a,$b,$c"
}
"#;
    if let Some(out) = run_box("SuspendTailForwardEarlyReturn", body) {
        assert_eq!(out, "OK", "early returns in a tail-forwarded suspend fn");
    }
}

#[test]
fn suspend_extension_function_on_user_type() {
    // A suspend extension function on a user class; the receiver is threaded alongside the continuation,
    // and a suspend call inside the extension body suspends against it.
    let body = r#"
class Counter(val base: Int)
suspend fun bump(x: Int): Int = x + 1
suspend fun Counter.next(): Int {
    val b = bump(base)
    return b + 1
}
suspend fun compute(): Int {
    val c = Counter(40)
    return c.next()
}
fun box(): String {
    val r = runBlocking { compute() }
    return if (r == 42) "OK" else "FAIL:$r"
}
"#;
    if let Some(out) = run_box("SuspendExtension", body) {
        assert_eq!(out, "OK", "suspend extension function on user type");
    }
}
