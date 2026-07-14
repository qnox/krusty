//! A `try { … } finally { … }` whose TRY BODY suspends (so the coroutine pass flattens it across
//! states). The `finally` must run on EVERY exit from the try body: normal completion and an
//! exception propagating out. Covers a non-suspending finally (the common `unlock()`/cleanup shape).
//! Needs the JVM toolchain + kotlin-stdlib + coroutines + real kotlinc.
use super::common;

#[test]
fn suspend_try_finally_body_runs() {
    let Some(jdk) = common::jdk_modules() else {
        return;
    };
    let Some(sl) = common::stdlib_jar() else {
        return;
    };
    let Some(coro) = common::coroutines_jar() else {
        return;
    };
    // `d` is a (trivially) suspending call so the try body suspends and is flattened.
    //   normal():   log "a" then value d(n)=7, then finally "f"        -> "af", r=7
    //   thrown():   log "b", throw, finally "F" runs, caught outside    -> "bF", caught
    //   value():    try-as-expression value with a suspending tail + finally
    const MAIN: &str = "import kotlinx.coroutines.runBlocking\n\
        suspend fun d(x: Int): Int = x\n\
        val log = StringBuilder()\n\
        suspend fun normal(n: Int): Int =\n\
            try { log.append(\"a\"); d(n) } finally { log.append(\"f\") }\n\
        suspend fun thrown(): Int {\n\
            return try { log.append(\"b\"); d(1); throw RuntimeException(\"x\") } finally { log.append(\"F\") }\n\
        }\n\
        fun box(): String = runBlocking {\n\
            val a = normal(7)\n\
            var caught = false\n\
            try { thrown() } catch (e: RuntimeException) { caught = true }\n\
            if (a == 7 && caught && log.toString() == \"afbF\") \"OK\"\n\
            else \"F a=$a caught=$caught log=$log\"\n\
        }\n";
    let out = common::compile_and_run_box(MAIN, "Main", &[sl, coro, jdk.clone()], Some(&jdk));
    assert_eq!(
        out.as_deref(),
        Some("OK"),
        "finally must run on normal completion AND on exception through a suspending try body"
    );
}
