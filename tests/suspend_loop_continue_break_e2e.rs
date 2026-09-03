//! `continue`/`break` inside a SUSPENDING loop (a loop whose body suspends, so the coroutine pass
//! flattens it across states). A structured `continue`/`break` can't survive flattening — at emit it
//! would target the dispatch `while(true)` loop, not the user's logical loop — so the pass rewrites each
//! jump to a `goto` to the loop's continue/break state. Covers a bare jump, a jump in an `if`, and the
//! expression-position `?: continue` / `?: break` (elvis whose else-branch diverges), across `for`
//! (counted range) / `while` / `do`-`while`, plus nested SUSPENDING loops. Production hit:
//! `ResourceAggregationService` (`val app = appCatalog[id] ?: continue` in a suspend for-loop).
//! Needs the JVM toolchain + kotlin-stdlib + coroutines + real kotlinc; skips otherwise.
use super::common;

#[test]
fn suspend_loop_continue_break_runs() {
    let jdk = common::jdk_modules();
    let sl = common::stdlib_jar();
    let coro = common::coroutines_jar();
    // Each callee suspends (completes synchronously under runBlocking) so every loop body suspends and
    // is flattened; the jumps therefore exercise the state-machine goto rewrite, not structural emit.
    //   forSkip:  0..5 skip i==2                 → 0+1+3+4+5   = 13
    //   forBreak: 0..5 break at i==3             → 0+1+2       = 3
    //   whileCont: i 1..6, skip i==2             → 1+3+4+5+6   = 19
    //   doBreak:  i from 1, break at 4           → 1+2+3       = 6
    //   elvisCont: keys present in g only        → 10+30       = 40
    //   elvisBreak: add 0→5, 1→10, break at 2    → 5+10        = 15
    //   nested:   i,j in 0..2, skip j==1         → Σ(i*10+j)   = 66
    const MAIN: &str = "import kotlinx.coroutines.runBlocking\n\
        suspend fun d(x: Int): Int = x\n\
        suspend fun forSkip(): Int { var s = 0; for (i in 0..5) { if (i == 2) continue; s += d(i) }; return s }\n\
        suspend fun forBreak(): Int { var s = 0; for (i in 0..5) { if (i == 3) break; s += d(i) }; return s }\n\
        suspend fun whileCont(): Int { var s = 0; var i = 0; while (i < 6) { i++; if (i == 2) continue; s += d(i) }; return s }\n\
        suspend fun doBreak(): Int { var s = 0; var i = 0; do { i++; if (i == 4) break; s += d(i) } while (i < 9); return s }\n\
        suspend fun elvisCont(m: Map<Int, Int>): Int { var s = 0; for (i in 0..5) { val v = m[i] ?: continue; s += d(v) }; return s }\n\
        suspend fun elvisBreak(m: Map<Int, Int>): Int { var s = 0; for (i in 0..5) { val v = m[i] ?: break; s += d(v) }; return s }\n\
        suspend fun nested(): Int { var s = 0; for (i in 0..2) { for (j in 0..2) { if (j == 1) continue; s += d(i * 10 + j) } }; return s }\n\
        fun box(): String = runBlocking {\n\
            val a = forSkip(); val b = forBreak(); val c = whileCont(); val e = doBreak()\n\
            val f = elvisCont(mapOf(1 to 10, 3 to 30)); val g = elvisBreak(mapOf(0 to 5, 1 to 10))\n\
            val h = nested()\n\
            if (a == 13 && b == 3 && c == 19 && e == 6 && f == 40 && g == 15 && h == 66) \"OK\"\n\
            else \"F a=$a b=$b c=$c e=$e f=$f g=$g h=$h\"\n\
        }\n";
    let out =
        common::compile_and_run_box(MAIN, "Main", &[sl, coro, jdk.clone()], Some(jdk.as_path()));
    assert_eq!(
        out.as_deref(),
        Some("OK"),
        "continue/break (bare, in-if, elvis) across suspend for/while/do-while + nested loops"
    );
}

#[test]
fn suspend_loop_nested_elvis_fallback_exposes_break_and_continue() {
    let jdk = common::jdk_modules();
    let sl = common::stdlib_jar();
    let coro = common::coroutines_jar();
    const MAIN: &str = "import kotlinx.coroutines.runBlocking\n\
        suspend fun maybe(i: Int): String? = if (i == 0) null else \"\"\n\
        suspend fun continueCase(): Int {\n\
            var i = 0; var hits = 0\n\
            while (i < 3) {\n\
                val value = maybe(i) ?: if (i == 0) { i++; continue } else error(\"bad\")\n\
                if (value != \"\") error(\"value\")\n\
                hits++; i++\n\
            }\n\
            return hits\n\
        }\n\
        suspend fun breakCase(): Boolean {\n\
            var seen = false\n\
            while (true) {\n\
                val value = maybe(0) ?: if (!seen) { seen = true; break } else error(\"bad\")\n\
                error(\"unexpected: $value\")\n\
            }\n\
            return seen\n\
        }\n\
        fun box(): String = runBlocking {\n\
            if (continueCase() == 2 && breakCase()) \"OK\" else \"FAIL\"\n\
        }\n";
    let out = common::expect_box_run(MAIN, "Main", &[sl, coro, jdk.clone()], Some(jdk.as_path()));
    assert_eq!(out, "OK");
}

#[test]
fn nested_elvis_continue_runs_suspending_loop_finally() {
    let jdk = common::jdk_modules();
    let sl = common::stdlib_jar();
    let coro = common::coroutines_jar();
    const MAIN: &str = "import kotlinx.coroutines.runBlocking\n\
        suspend fun maybe(i: Int): String? = if (i == 0) null else \"\"\n\
        suspend fun test(): String {\n\
            var i = 0; var cleanup = 0\n\
            while (i < 3) {\n\
                try {\n\
                    val value = maybe(i) ?: if (i == 0) { i++; continue } else error(\"bad\")\n\
                    if (value != \"\") error(\"value\")\n\
                    i++\n\
                } finally { cleanup++ }\n\
            }\n\
            return \"$i:$cleanup\"\n\
        }\n\
        fun box(): String = runBlocking { if (test() == \"3:3\") \"OK\" else \"FAIL:${test()}\" }\n";
    let out = common::expect_box_run(MAIN, "Main", &[sl, coro, jdk.clone()], Some(jdk.as_path()));
    assert_eq!(out, "OK");
}

#[test]
fn suspending_when_conditions_remain_left_to_right_and_short_circuit() {
    let jdk = common::jdk_modules();
    let sl = common::stdlib_jar();
    let coro = common::coroutines_jar();
    const MAIN: &str = "import kotlinx.coroutines.runBlocking\n\
        var log = \"\"\n\
        suspend fun condition(mark: String, answer: Boolean): Boolean { log += mark; return answer }\n\
        suspend fun select(): String {\n\
            val first = when {\n\
                condition(\"a\", true) -> \"first\"\n\
                condition(\"b\", true) -> \"wrong\"\n\
                else -> \"wrong-else\"\n\
            }\n\
            val second = when {\n\
                condition(\"c\", false) -> \"wrong\"\n\
                condition(\"d\", true) -> \"second\"\n\
                condition(\"e\", true) -> \"wrong\"\n\
                else -> \"wrong-else\"\n\
            }\n\
            return \"$first:$second:$log\"\n\
        }\n\
        fun box(): String = runBlocking { if (select() == \"first:second:acd\") \"OK\" else \"FAIL:$log\" }\n";
    let out = common::expect_box_run(MAIN, "Main", &[sl, coro, jdk.clone()], Some(jdk.as_path()));
    assert_eq!(out, "OK");
}
