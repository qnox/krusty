//! A lambda's non-local `return` out of an inline function that holds the lambda inside a
//! `try`/`finally` (`synchronized`, `withLock`, `ReentrantReadWriteLock.write`). kotlinc keeps the
//! return a return of the caller and copies every `finally` it leaves in front of it: the inlined
//! function's own (`InternalFinallyBlockInliner`) and then the caller's
//! (`generateAndInsertFinallyBlocks`). Without the copies the lock is never released.
use super::common;

const BOX: &str = "import java.util.concurrent.locks.ReentrantLock\n\
import java.util.concurrent.locks.ReentrantReadWriteLock\n\
import kotlin.concurrent.withLock\n\
import kotlin.concurrent.write\n\
val log = StringBuilder()\n\
class Lk : ReentrantLock() { override fun unlock() { log.append(\"u\"); super.unlock() } }\n\
fun nested(a: Any, b: Any, x: String?): String {\n\
    synchronized(a) { synchronized(b) { if (x == null) return \"n\" } }\n\
    return x!!\n\
}\n\
fun ownFinally(l: Lk, x: Int): Int {\n\
    l.withLock { try { if (x > 0) return x } finally { log.append(\"f\") } }\n\
    return -1\n\
}\n\
fun wide(l: Lk, x: Long): Long { l.withLock { if (x > 0) return x * 2 }; return 0L }\n\
fun unit(l: Lk, x: Int) { l.withLock { if (x > 0) return }; log.append(\"a\") }\n\
fun rw(l: ReentrantReadWriteLock, x: Int): Int { l.write { if (x > 0) return x + 1 }; return 0 }\n\
fun always(l: Lk): String { l.withLock { return \"always\" } }\n\
fun loop(l: Lk, xs: List<Int>): Int { for (x in xs) { l.withLock { if (x == 3) return x } }; return -1 }\n\
fun outer(l: Lk, x: Int): Int { try { l.withLock { if (x > 0) return x } } finally { log.append(\"o\") }; return 0 }\n\
fun outerAlways(l: Lk): Int { try { l.withLock { return 4 } } finally { log.append(\"o\") } }\n\
fun box(): String {\n\
    val l = Lk()\n\
    val r = StringBuilder()\n\
    r.append(nested(Any(), Any(), null)).append(nested(Any(), Any(), \"y\"))\n\
    r.append(ownFinally(l, 5)).append(ownFinally(l, -5))\n\
    r.append(wide(l, 3L))\n\
    unit(l, 1); unit(l, 0)\n\
    val rwl = ReentrantReadWriteLock()\n\
    r.append(rw(rwl, 1)).append(rw(rwl, 0)).append(rwl.isWriteLocked)\n\
    r.append(always(l)).append(loop(l, listOf(1, 2, 3, 4)))\n\
    r.append(outer(l, 1)).append(outer(l, 0)).append(outerAlways(l))\n\
    r.append(l.isLocked).append(\" \").append(log)\n\
    val result = r.toString()\n\
    return if (result == \"ny5-1620falsealways3104false fufuuuuauuuuuououo\") \"OK\" else \"fail: $result\"\n\
}\n";

#[test]
fn a_non_local_return_runs_every_finally_it_leaves() {
    common::expect_box_same_as_kotlinc(BOX, "InlineNonLocalReturnFinally");
}

#[test]
fn conditional_non_local_returns_through_finally_are_byte_identical() {
    let src = "import java.util.concurrent.locks.ReentrantLock\n\
               import kotlin.concurrent.withLock\n\
               fun nested(a: Any, b: Any, x: String?): String {\n\
               \x20   synchronized(a) { synchronized(b) { if (x == null) return \"n\" } }\n\
               \x20   return x!!\n\
               }\n\
               fun ownFinally(l: ReentrantLock, x: Int, log: StringBuilder): Int {\n\
               \x20   l.withLock { try { if (x > 0) return x } finally { log.append(\"f\") } }\n\
               \x20   return -1\n\
               }\n\
               fun wide(l: ReentrantLock, x: Long): Long { l.withLock { if (x > 0) return x * 2 }; return 0L }\n\
               fun dbl(l: ReentrantLock, x: Double): Double { l.withLock { if (x > 0) return x / 2 }; return 0.0 }\n\
               fun unit(l: ReentrantLock, x: Int, log: StringBuilder) { l.withLock { if (x > 0) return }; log.append(\"a\") }\n\
               fun loop(l: ReentrantLock, xs: List<Int>): Int {\n\
               \x20   for (x in xs) { l.withLock { if (x == 3) return x } }\n\
               \x20   return -1\n\
               }\n\
               fun outer(l: ReentrantLock, x: Int, log: StringBuilder): Int {\n\
               \x20   try { l.withLock { if (x > 0) return x } } finally { log.append(\"o\") }\n\
               \x20   return 0\n\
               }\n";
    common::byte_diff_against_kotlinc_cp(
        "NonLocalReturnFinally",
        src,
        "NonLocalReturnFinallyKt",
        &[common::stdlib_jar()],
    )
    .expect("reference kotlinc is provisioned")
    .expect("NonLocalReturnFinallyKt byte-identical to kotlinc");
}

/// A `break` or `continue` of the caller's loop inside the value a lambda returns non-locally
/// (`return if (…) seen else continue`): the jump decides the lambda's route, not the return.
const JUMP_IN_RETURNED_VALUE: &str = "fun firstOver(xs: List<Int>, limit: Int): Int {\n\
    var seen = 0\n\
    for (x in xs) {\n\
        run {\n\
            seen += x\n\
            return if (x > limit) seen else if (x % 2 == 0) continue else break\n\
        }\n\
    }\n\
    return -seen\n\
}\n\
fun box(): String {\n\
    val r = \"\" + firstOver(listOf(2, 4, 9), 5) + firstOver(listOf(2, 3, 9), 5) + firstOver(listOf(2, 4), 5)\n\
    return if (r == \"15-5-6\") \"OK\" else r\n\
}\n";

#[test]
fn a_jump_inside_a_returned_value_runs() {
    common::expect_box_same_as_kotlinc(JUMP_IN_RETURNED_VALUE, "JumpInReturnedValue");
}

#[test]
fn a_jump_inside_a_returned_value_is_byte_identical() {
    common::byte_diff_against_kotlinc_cp(
        "JumpInReturnedValue",
        JUMP_IN_RETURNED_VALUE,
        "JumpInReturnedValueKt",
        &[common::stdlib_jar()],
    )
    .expect("reference kotlinc is provisioned")
    .expect("JumpInReturnedValueKt byte-identical to kotlinc");
}
