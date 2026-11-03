//! A `break` or `continue` of the caller's loop out of a lambda passed to a library inline function
//! that holds the lambda inside a `try`/`finally` (`withLock`, `synchronized`). kotlinc compiles
//! the jump as a non-local return naming the loop, so the inlined function's `finally` blocks are
//! copied in front of it, and then writes it into the caller as the jump to the loop, behind the
//! caller's `finally` blocks it leaves. Without the copies the lock is never released.
use super::common;

const BOX: &str = "import java.util.concurrent.locks.ReentrantLock\n\
import kotlin.concurrent.withLock\n\
val log = StringBuilder()\n\
class Lk : ReentrantLock() { override fun unlock() { log.append(\"u\"); super.unlock() } }\n\
fun firstEven(l: Lk, xs: List<Int>): Int {\n\
    for (x in xs) { l.withLock { if (x < 0) continue; if (x % 2 == 0) return x } }\n\
    return -1\n\
}\n\
fun breaks(l: Lk, xs: List<Int>): Int {\n\
    var r = 0\n\
    for (x in xs) { l.withLock { if (x == 3) break; r += x } }\n\
    return r\n\
}\n\
fun monitor(a: Any, xs: List<Int>): String {\n\
    var r = \"\"\n\
    for (x in xs) { synchronized(a) { if (x == 2) continue; if (x == 4) break; r += x } }\n\
    return r + Thread.holdsLock(a)\n\
}\n\
fun callerFinally(l: Lk, xs: List<Int>): Int {\n\
    var n = 0\n\
    for (x in xs) {\n\
        try { l.withLock { if (x == 1) continue; if (x == 3) break; n += x } } finally { log.append(\"o\") }\n\
    }\n\
    return n\n\
}\n\
fun labeled(l: Lk, rows: List<List<Int>>): Int {\n\
    var n = 0\n\
    outer@ for (row in rows) {\n\
        for (x in row) { l.withLock { if (x < 0) continue@outer; if (x == 0) break@outer; n += x } }\n\
    }\n\
    return n\n\
}\n\
fun nested(l: Lk, a: Any, xs: List<Int>): String {\n\
    var r = \"\"\n\
    for (x in xs) { l.withLock { synchronized(a) { if (x == 2) continue; if (x == 3) break; r += x } } }\n\
    return r + Thread.holdsLock(a)\n\
}\n\
fun ownFinally(l: Lk, xs: List<Int>): Int {\n\
    var n = 0\n\
    for (x in xs) { l.withLock { try { if (x == 2) continue; if (x == 3) break; n += x } finally { log.append(\"f\") } } }\n\
    return n\n\
}\n\
fun whileLoop(l: Lk, limit: Int): Int {\n\
    var i = 0\n\
    var n = 0\n\
    while (i < limit) { i++; l.withLock { if (i % 2 == 0) continue; n += i } }\n\
    return n\n\
}\n\
fun box(): String {\n\
    val l = Lk()\n\
    val a = Any()\n\
    val r = StringBuilder()\n\
    r.append(firstEven(l, listOf(1, -3, 4, 5))).append(' ')\n\
    r.append(breaks(l, listOf(1, 2, 3, 4))).append(' ')\n\
    r.append(monitor(a, listOf(1, 2, 3, 4, 5))).append(' ')\n\
    r.append(callerFinally(l, listOf(1, 2, 3, 4))).append(' ')\n\
    r.append(labeled(l, listOf(listOf(1, 2), listOf(3, -1, 7), listOf(4, 0, 9), listOf(8)))).append(' ')\n\
    r.append(nested(l, a, listOf(1, 2, 3, 4))).append(' ')\n\
    r.append(ownFinally(l, listOf(1, 2, 3, 4))).append(' ')\n\
    r.append(whileLoop(l, 6)).append(' ')\n\
    r.append(l.isLocked).append(' ').append(log)\n\
    val expected = \"4 3 13false 2 10 1false 1 9 false uuuuuuuouououuuuuuuuufufufuuuuuuu\"\n\
    return if (r.toString() == expected) \"OK\" else r.toString()\n\
}\n";

#[test]
fn a_non_local_jump_runs_every_finally_it_leaves() {
    common::expect_box_same_as_kotlinc(BOX, "InlineNonLocalJumpFinally");
}

#[test]
fn non_local_jumps_through_finally_are_byte_identical() {
    common::byte_diff_against_kotlinc_cp(
        "InlineNonLocalJumpFinally",
        BOX,
        "InlineNonLocalJumpFinallyKt",
        &[common::stdlib_jar()],
    )
    .expect("reference kotlinc is provisioned")
    .expect("InlineNonLocalJumpFinallyKt byte-identical to kotlinc");
}

/// One literal that both returns from an enclosing inline expansion (`return@outer`) and breaks or
/// continues the caller's loop: each way out runs the inlined function's `finally`. Compared with
/// kotlinc's run only: kotlinc parks the returned value below `withLock`'s catch local here, a slot
/// order krusty does not reproduce inside a same-module inline lambda yet.
const MIXED: &str = "import java.util.concurrent.locks.ReentrantLock\n\
import kotlin.concurrent.withLock\n\
val log = StringBuilder()\n\
class Lk : ReentrantLock() { override fun unlock() { log.append(\"u\"); super.unlock() } }\n\
inline fun <T> ownRun(f: () -> T): T = f()\n\
fun mixedBreak(l: Lk, xs: List<Int>): String {\n\
    var r = \"\"\n\
    for (x in xs) {\n\
        r += ownRun outer@{ l.withLock { if (x == 2) return@outer \"r\"; if (x == 4) break; x.toString() } }\n\
    }\n\
    return r\n\
}\n\
fun mixedContinue(l: Lk, xs: List<Int>): String {\n\
    var r = \"\"\n\
    for (x in xs) {\n\
        r += ownRun outer@{ l.withLock { if (x == 2) continue; if (x == 4) return@outer \"r\"; x.toString() } }\n\
    }\n\
    return r\n\
}\n\
fun frameOnly(l: Lk, xs: List<Int>): String {\n\
    var r = \"\"\n\
    for (x in xs) {\n\
        r += ownRun outer@{ l.withLock { if (x == 2) return@outer \"r\"; x.toString() } }\n\
    }\n\
    return r\n\
}\n\
fun box(): String {\n\
    val l = Lk()\n\
    val r = StringBuilder()\n\
    r.append(mixedBreak(l, listOf(1, 2, 3, 4, 5))).append(' ')\n\
    r.append(mixedContinue(l, listOf(1, 2, 3, 4, 5))).append(' ')\n\
    r.append(frameOnly(l, listOf(1, 2, 3))).append(' ')\n\
    r.append(l.isLocked).append(' ').append(l.holdCount).append(' ').append(log)\n\
    val expected = \"1r3 13r5 1r3 false 0 uuuuuuuuuuuu\"\n\
    return if (r.toString() == expected) \"OK\" else r.toString()\n\
}\n";

#[test]
fn a_literal_that_returns_from_an_enclosing_expansion_and_jumps_runs_every_finally() {
    common::expect_box_same_as_kotlinc(MIXED, "InlineMixedFrameReturnJump");
}
