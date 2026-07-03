//! `kotlinx.coroutines.runBlocking { … }` — the classpath coroutine driver — resolves, lowers, and RUNS
//! end-to-end against the real coroutines runtime. Two coordinated fixes made this work:
//!
//!  1. RESOLUTION (`call_resolver.rs`): `runBlocking { }` passes ONE trailing lambda but the function has
//!     two parameters (a defaulted `context` + the `block`). `default_omit_lambda_param_indices` aligns the
//!     trailing lambda to the LAST parameter (omitting leading defaults) so the checker's lambda helpers
//!     type the block and the call resolves to `BuildersKt.runBlocking$default`.
//!  2. LOWERING (`ir_lower.rs`): the block is `suspend CoroutineScope.() -> T`, erased in the descriptor to
//!     a bare `Function2` (no `suspend` flag). `lower_arg` detects the suspend lambda STRUCTURALLY (its
//!     checked type ends in a `Continuation`), strips it, and routes to `lower_suspend_lambda` — building a
//!     real `SuspendLambda` state machine (the CoroutineScope receiver is modeled as the value parameter).
//!     The lambda body is lowered as a suspend context (`cur_fn_suspend`) so a suspend MEMBER call inside
//!     (`repo.get(…)` on a classpath `suspend` interface) is CPS-threaded, and `suspend_member_call`
//!     detection consults the library for classpath members.
//!
//! Requires the coroutines runtime jar; skipped when it (or the toolchain) is unavailable.
mod common;

fn run(lib: Option<(&str, &str)>, main: &str) -> Option<String> {
    let jdk = common::jdk_modules()?;
    let sl = common::stdlib_jar()?;
    let corou = common::coroutines_jar()?;
    let mut cp = vec![sl, corou, jdk.clone()];
    if let Some((tag, src)) = lib {
        cp.insert(0, common::compile_lib(tag, src)?);
    }
    common::compile_and_run_box(main, "Main", &cp, Some(&jdk))
}

#[test]
fn runblocking_nonsuspending_body() {
    // A block that does not suspend still drives through `runBlocking` and returns its value.
    const SRC: &str = "import kotlinx.coroutines.runBlocking\n\
        fun box(): String {\n\
        \x20 val n = runBlocking { 20 + 22 }\n\
        \x20 return if (n == 42) \"OK\" else \"fail: $n\"\n\
        }\n";
    let Some(r) = run(None, SRC) else { return };
    assert_eq!(r, "OK");
}

#[test]
fn runblocking_tail_suspend_call() {
    // `runBlocking { work() }` — a single tail call to a top-level `suspend fun`.
    const SRC: &str = "import kotlinx.coroutines.runBlocking\n\
        suspend fun work(): Int = 42\n\
        fun box(): String {\n\
        \x20 val n = runBlocking { work() }\n\
        \x20 return if (n == 42) \"OK\" else \"fail: $n\"\n\
        }\n";
    let Some(r) = run(None, SRC) else { return };
    assert_eq!(r, "OK");
}

#[test]
fn runblocking_bound_suspend_then_use() {
    // `val x = work(); <use x>` — a bound suspension followed by a tail expression.
    const SRC: &str = "import kotlinx.coroutines.runBlocking\n\
        suspend fun work(): Int = 42\n\
        fun box(): String = runBlocking {\n\
        \x20 val x = work()\n\
        \x20 if (x == 42) \"OK\" else \"fail\"\n\
        }\n";
    let Some(r) = run(None, SRC) else { return };
    assert_eq!(r, "OK");
}

#[test]
fn runblocking_classpath_suspend_member() {
    // The w1 scenario end-to-end: a `suspend` interface member returning `List<CustomType>` (with a
    // `List<Int>` parameter), called and consumed inside `runBlocking`.
    const LIB: &str = "package lib\n\
        data class Info(val n: Int)\n\
        interface Port { suspend fun get(ids: List<Int>): List<Info> }\n\
        class RealPort : Port {\n\
        \x20 override suspend fun get(ids: List<Int>): List<Info> = ids.map { Info(it * 10) }\n\
        }\n";
    const SRC: &str =
        "import lib.Port\nimport lib.RealPort\nimport kotlinx.coroutines.runBlocking\n\
        fun box(): String = runBlocking {\n\
        \x20 val p: Port = RealPort()\n\
        \x20 val xs = p.get(listOf(1, 2, 3))\n\
        \x20 val s = xs.sumOf { it.n }\n\
        \x20 if (s == 60) \"OK\" else \"fail: $s\"\n\
        }\n";
    let Some(r) = run(Some(("rb_port", LIB)), SRC) else {
        return;
    };
    assert_eq!(r, "OK");
}
