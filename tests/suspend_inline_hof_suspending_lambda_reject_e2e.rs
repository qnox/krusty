//! SAFETY GUARD: a non-inlined `suspend inline fun` call (e.g. `kotlinx.coroutines.sync.Mutex.withLock`)
//! whose LAMBDA ARGUMENT itself SUSPENDS must be cleanly DECLINED, never miscompiled.
//!
//! `withLock` is `public suspend inline fun <T> Mutex.withLock(owner: Any? = null, action: () -> T): T`.
//! krusty does NOT splice it — it lowers the call as a plain `MutexKt.withLock$default(mutex, owner,
//! Function0, cont)`, passing the lambda as a NON-suspend `Function0`. That is fine when the lambda body
//! does not suspend (see `build840_nn1`). When the body SUSPENDS, a plain `Function0.invoke()` cannot call
//! a suspend function, so the emitted closure is INVALID bytecode (`java -Xverify:all` →
//! `VerifyError: Operand stack underflow`) even though krusty exits 0 — a forbidden miscompile.
//!
//! Until general suspend-inline splicing lands (inline the lock/try/finally body and splice the user lambda
//! into the enclosing CPS state machine, as kotlinc does), krusty must BAIL for this shape rather than emit
//! bad code. This is a GENERIC guard on "non-inlined suspend inline fun + suspending lambda arg", not a
//! `withLock` special case. If this starts compiling, the real feature has landed — promote to a
//! round-trip test that runs the locked block under `runBlocking`.

use super::common;

/// Compile `src` through the frontend + JVM backend in-process with the coroutines jar on the classpath.
/// Returns `true` when the backend cleanly declines (no bytecode emitted). Skip-clean (`true`) when the
/// toolchain is absent so the suite never fails on a machine without the vendored kotlinc/JDK/coroutines.
fn rejects(src: &str) -> bool {
    let (Some(stdlib), Some(coro), Some(jdk)) = (
        common::stdlib_jar(),
        common::coroutines_jar(),
        common::jdk_modules(),
    ) else {
        return true;
    };
    common::backend_rejects_in_process(src, "S", &[stdlib, coro], Some(&jdk)).unwrap_or(false)
}

/// The exact miscompile from the bug report: a `?.let { return@withLock … }` non-local return plus a
/// trailing suspend call (`make()`) inside the `withLock` lambda. Previously emitted invalid bytecode.
#[test]
fn withlock_lambda_with_suspend_and_nonlocal_return_rejected() {
    assert!(rejects(
        "import kotlinx.coroutines.sync.Mutex\n\
         import kotlinx.coroutines.sync.withLock\n\
         suspend fun make(): String = \"x\"\n\
         suspend fun f(m: Mutex, s: String?): String = m.withLock { s?.let { return@withLock it }; make() }\n"
    ));
}

/// The minimal shape: the `withLock` lambda body is a single suspend call. A plain `Function0` still
/// cannot call it, so this must decline too.
#[test]
fn withlock_lambda_with_single_suspend_call_rejected() {
    assert!(rejects(
        "import kotlinx.coroutines.sync.Mutex\n\
         import kotlinx.coroutines.sync.withLock\n\
         suspend fun make(): String = \"x\"\n\
         suspend fun f(m: Mutex): String = m.withLock { make() }\n"
    ));
}

/// GUARD AGAINST OVER-BAIL: a `withLock` lambda whose body does NOT suspend must still compile — the plain
/// `Function0` path is correct there. (Mirrors `build840_nn1`; kept here so a too-broad guard is caught.)
#[test]
fn withlock_lambda_without_suspension_still_compiles() {
    assert!(!rejects(
        "import kotlinx.coroutines.sync.Mutex\n\
         import kotlinx.coroutines.sync.withLock\n\
         import kotlinx.coroutines.runBlocking\n\
         fun box(): String = runBlocking { val m = Mutex(); val r = m.withLock { 42 }; if (r == 42) \"OK\" else \"F\" }\n"
    ));
}
