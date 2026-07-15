//! A suspend call buried in a compound-assignment inside a loop body
//! (`while`/`do-while`/`for` over a range) — e.g. `while (s < 3) { s += d() }`. The suspension sits in
//! the RHS of `s += d()`, an ordinary loop statement; the coroutine pass hoists it to a temp
//! (`val t = d(); s += t`) so the state machine can resume across it. Earlier krusty declined these
//! shapes (guarded by `backend_rejection_coverage`/`inline_vc_suspend_coverage` rejection tests); the
//! hoist now models them, so this is the promoted round-trip test that proves they run correctly.
//! Needs the JVM toolchain + kotlin-stdlib + coroutines + real kotlinc; skips otherwise.
use super::common;

#[test]
fn suspend_compound_assign_in_loops_runs() {
    let Some(jdk) = common::jdk_modules() else {
        return;
    };
    let Some(sl) = common::stdlib_jar() else {
        return;
    };
    let Some(coro) = common::coroutines_jar() else {
        return;
    };
    // d() suspends and returns 1; weight(i) suspends and returns i*2.
    //   whileSum:   0 -> 1 -> 2 -> 3          == 3
    //   doWhileSum: 1 -> 2 -> 3               == 3
    //   forRange:   d() thrice (0..2)         == 3
    //   forUntil:   weight(0)+weight(1)+weight(2) = 0+2+4 == 6
    const MAIN: &str = "import kotlinx.coroutines.runBlocking\n\
        suspend fun d(): Int = 1\n\
        suspend fun weight(i: Int): Int = i * 2\n\
        suspend fun whileSum(): Int { var s = 0; while (s < 3) { s += d() }; return s }\n\
        suspend fun doWhileSum(): Int { var i = 0; do { i += d() } while (i < 3); return i }\n\
        suspend fun forRange(): Int { var s = 0; for (i in 0..2) { s += d() }; return s }\n\
        suspend fun forUntil(n: Int): Int { var s = 0; for (i in 0 until n) { s += weight(i) }; return s }\n\
        suspend fun whileMulti(n: Int): Int { var s = 0; var i = 0; while (i < n) { s += d(); i++ }; return s }\n\
        fun box(): String = runBlocking {\n\
            val a = whileSum(); val b = doWhileSum(); val c = forRange()\n\
            val e = forUntil(3); val f = whileMulti(3)\n\
            if (a == 3 && b == 3 && c == 3 && e == 6 && f == 3) \"OK\"\n\
            else \"F a=$a b=$b c=$c e=$e f=$f\"\n\
        }\n";
    let out = common::compile_and_run_box(MAIN, "Main", &[sl, coro, jdk.clone()], Some(&jdk));
    assert_eq!(
        out.as_deref(),
        Some("OK"),
        "suspend compound-assign in while/do-while/for loops"
    );
}
