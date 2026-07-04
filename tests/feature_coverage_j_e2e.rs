//! End-to-end `box()` coverage for `suspend`/coroutine lowering (`src/jvm/suspend.rs`).
//!
//! Each test compiles a self-contained Kotlin program whose `fun box(): String` drives one or more
//! `suspend` functions through a local `runBlocking` (a synchronous coroutine driver built on
//! `startCoroutine`, mirroring the box-corpus `CoroutineUtil.kt`) and returns "OK". The program is
//! compiled in-process and its `box()` runs on the persistent JVM via `common::compile_and_run_box`.
//! All suspend callees complete synchronously, so the whole state machine runs to completion under
//! bytecode verification — proving krusty's CPS lowering, spilling and resumption are byte-correct.
//!
//! These are DIFFERENT scenarios from `tests/suspend_e2e.rs` (which drives the raw CPS ABI from a
//! hand-written Java `Continuation`): here we exercise the end-user `runBlocking { … }` surface.

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
fn leaf_suspend_returns_value() {
    // A leaf `suspend fun` (no suspension point) called from `runBlocking`.
    let body = r#"
suspend fun answer(): Int = 42
fun box(): String {
    val r = runBlocking { answer() }
    return if (r == 42) "OK" else "FAIL:$r"
}
"#;
    if let Some(out) = run_box("LeafSuspendValue", body) {
        assert_eq!(out, "OK", "leaf suspend value");
    }
}

#[test]
fn sequential_suspend_calls_accumulate() {
    // Several sequential suspend calls whose results accumulate across suspension points (each result
    // must be spilled and reloaded as the state machine advances).
    let body = r#"
suspend fun one(): Int = 1
suspend fun two(): Int = 2
suspend fun three(): Int = 3
suspend fun total(): Int {
    val a = one()
    val b = two()
    val c = three()
    return a + b + c
}
fun box(): String {
    val r = runBlocking { total() }
    return if (r == 6) "OK" else "FAIL:$r"
}
"#;
    if let Some(out) = run_box("SequentialSuspend", body) {
        assert_eq!(out, "OK", "sequential suspend accumulation");
    }
}

#[test]
fn nested_suspend_call() {
    // A suspend fun that calls another suspend fun, which itself calls a third — nested CPS threading.
    let body = r#"
suspend fun base(): Int = 10
suspend fun middle(): Int {
    val b = base()
    return b + 5
}
suspend fun outer(): Int {
    val m = middle()
    return m * 2
}
fun box(): String {
    val r = runBlocking { outer() }
    return if (r == 30) "OK" else "FAIL:$r"
}
"#;
    if let Some(out) = run_box("NestedSuspend", body) {
        assert_eq!(out, "OK", "nested suspend call");
    }
}

#[test]
fn suspend_loop_with_suspend_call() {
    // A `while` loop body containing a suspend call; the accumulator is spilled across each iteration's
    // suspension point.
    let body = r#"
suspend fun step(x: Int): Int = x + 2
suspend fun run(): Int {
    var acc = 0
    var i = 0
    while (i < 5) {
        acc = step(acc)
        i = i + 1
    }
    return acc
}
fun box(): String {
    val r = runBlocking { run() }
    return if (r == 10) "OK" else "FAIL:$r"
}
"#;
    if let Some(out) = run_box("SuspendWhileLoop", body) {
        assert_eq!(out, "OK", "while loop with suspend call");
    }
}

#[test]
fn suspend_for_range_loop_with_suspend_call() {
    // A `for` over an `IntRange` with a suspend call in the body — the range's counter must be spilled
    // across the suspension point on every iteration.
    let body = r#"
suspend fun add(a: Int, b: Int): Int = a + b
suspend fun sumTo(n: Int): Int {
    var acc = 0
    for (i in 1..n) {
        acc = add(acc, i)
    }
    return acc
}
fun box(): String {
    val r = runBlocking { sumTo(4) }
    return if (r == 10) "OK" else "FAIL:$r"
}
"#;
    if let Some(out) = run_box("SuspendForLoop", body) {
        assert_eq!(out, "OK", "for-range loop with suspend call");
    }
}

#[test]
fn suspend_if_branch_around_suspend_calls() {
    // Branching (`if`/`else`) selecting between two suspend calls; each arm suspends, only the taken
    // one runs.
    let body = r#"
suspend fun yes(): Int = 100
suspend fun no(): Int = 200
suspend fun choose(c: Boolean): Int {
    return if (c) yes() else no()
}
fun box(): String {
    val a = runBlocking { choose(true) }
    val b = runBlocking { choose(false) }
    return if (a == 100 && b == 200) "OK" else "FAIL:$a,$b"
}
"#;
    if let Some(out) = run_box("SuspendIfBranch", body) {
        assert_eq!(out, "OK", "if-branch around suspend calls");
    }
}

#[test]
fn suspend_when_branch_around_suspend_calls() {
    // A `when` whose arms are suspend calls returning strings.
    let body = r#"
suspend fun first(): String = "a"
suspend fun second(): String = "b"
suspend fun third(): String = "c"
suspend fun pick(n: Int): String {
    return when (n) {
        0 -> first()
        1 -> second()
        else -> third()
    }
}
fun box(): String {
    val r = runBlocking { pick(0) + pick(1) + pick(2) }
    return if (r == "abc") "OK" else "FAIL:$r"
}
"#;
    if let Some(out) = run_box("SuspendWhenBranch", body) {
        assert_eq!(out, "OK", "when-branch around suspend calls");
    }
}

#[test]
fn suspend_returns_long() {
    // Suspend fun returning a `Long` (wide primitive; boxed as java.lang.Long through the CPS return).
    let body = r#"
suspend fun bigStep(x: Long): Long = x + 1000L
suspend fun compute(): Long {
    val a = bigStep(0L)
    val b = bigStep(a)
    return a + b
}
fun box(): String {
    val r = runBlocking { compute() }
    return if (r == 3000L) "OK" else "FAIL:$r"
}
"#;
    if let Some(out) = run_box("SuspendReturnsLong", body) {
        assert_eq!(out, "OK", "suspend returning Long");
    }
}

#[test]
fn suspend_returns_boolean() {
    // Suspend fun returning a `Boolean`, combined across two suspension points.
    let body = r#"
suspend fun isEven(x: Int): Boolean = x % 2 == 0
suspend fun check(): Boolean {
    val a = isEven(4)
    val b = isEven(7)
    return a && !b
}
fun box(): String {
    val r = runBlocking { check() }
    return if (r == true) "OK" else "FAIL:$r"
}
"#;
    if let Some(out) = run_box("SuspendReturnsBoolean", body) {
        assert_eq!(out, "OK", "suspend returning Boolean");
    }
}

#[test]
fn suspend_returns_string() {
    // Suspend fun returning a `String` (reference type; no boxing, but flows through Object-erased CPS).
    let body = r#"
suspend fun greet(name: String): String = "Hello, " + name
suspend fun build(): String {
    val a = greet("world")
    return a + "!"
}
fun box(): String {
    val r = runBlocking { build() }
    return if (r == "Hello, world!") "OK" else "FAIL:$r"
}
"#;
    if let Some(out) = run_box("SuspendReturnsString", body) {
        assert_eq!(out, "OK", "suspend returning String");
    }
}

#[test]
fn suspend_param_used_after_suspension() {
    // A parameter read AFTER a suspension point: `x` must be spilled into the continuation and reloaded
    // when the state machine resumes past `other()`.
    let body = r#"
suspend fun other(): Int = 10
suspend fun compute(x: Int): Int {
    val a = other()
    return x + a
}
fun box(): String {
    val r = runBlocking { compute(5) }
    return if (r == 15) "OK" else "FAIL:$r"
}
"#;
    if let Some(out) = run_box("SuspendParamAfter", body) {
        assert_eq!(out, "OK", "param used after suspension");
    }
}

#[test]
fn suspend_function_type_parameter_invoked() {
    // A `suspend () -> Int` function-type PARAMETER, invoked from inside another suspend fun; the
    // callee threads its own continuation into the passed suspend lambda.
    let body = r#"
suspend fun apply(block: suspend () -> Int): Int {
    val v = block()
    return v + 1
}
suspend fun leaf(): Int = 41
fun box(): String {
    val r = runBlocking { apply { leaf() } }
    return if (r == 42) "OK" else "FAIL:$r"
}
"#;
    if let Some(out) = run_box("SuspendFnTypeParam", body) {
        assert_eq!(out, "OK", "suspend function-type parameter");
    }
}

#[test]
fn try_catch_around_suspend_call() {
    // A `try`/`catch` around a suspend call. The happy path returns the suspended value; the throwing
    // path is caught and yields a fallback — the exception table must survive the state-machine split.
    let body = r#"
suspend fun ok(): Int = 7
suspend fun boom(): Int {
    throw RuntimeException("nope")
}
suspend fun guarded(fail: Boolean): Int {
    return try {
        if (fail) boom() else ok()
    } catch (e: RuntimeException) {
        -1
    }
}
fun box(): String {
    val a = runBlocking { guarded(false) }
    val b = runBlocking { guarded(true) }
    return if (a == 7 && b == -1) "OK" else "FAIL:$a,$b"
}
"#;
    if let Some(out) = run_box("SuspendTryCatch", body) {
        assert_eq!(out, "OK", "try/catch around suspend call");
    }
}
