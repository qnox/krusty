//! A statement `when` whose arms are blocks, in a suspending loop the state-machine flattener
//! splits. Normalizing an arm block for the flattener demotes its trailing value (`{ a++ }`) to a
//! statement, so the arm no longer yields the `when`'s checked result. A deeply exhaustive `when`
//! without `else` still recorded that result (`Int`), so emission kept a joined value no arm
//! produced and popped an empty stack: the frame computation declined with `Unsteppable` and the
//! compile panicked.
//!
//! The flattener (rather than the bytecode loop transformer) owns a suspend function the
//! transformer declines: a private one, or one whose body calls a classpath inline function. The
//! first test reaches it with repository-owned declarations only; the second is the shape a
//! private corpus hit, a `for ((name, cfg) in map)` loop over a sealed result.
use super::common;

#[test]
fn suspend_loop_statement_when_with_block_arms_matches_kotlinc() {
    const MAIN: &str = r#"import kotlin.coroutines.*

class Slot(val label: String, val weight: Int)

inline operator fun Slot.component1(): String = label
inline operator fun Slot.component2(): Int = weight

class Shelf(private val slots: Array<Slot>) {
    operator fun iterator(): Cursor = Cursor(slots)
}

class Cursor(private val slots: Array<Slot>) {
    private var at = 0
    operator fun hasNext(): Boolean = at < slots.size
    operator fun next(): Slot = slots[at++]
}

var parked: (() -> Unit)? = null

suspend fun heavy(weight: Int): Boolean =
    suspendCoroutine { c -> parked = { c.resume(weight > 2) } }

private suspend fun sort(shelf: Shelf): String {
    var light = 0
    var dense = 0
    var seen = ""
    for ((label, weight) in shelf) {
        when (heavy(weight)) {
            true -> { dense++ }
            false -> { light++ }
        }
        seen += label
    }
    return "$seen:$dense:$light"
}

fun box(): String {
    var outcome: Result<String>? = null
    suspend { sort(Shelf(arrayOf(Slot("a", 1), Slot("b", 3), Slot("c", 5)))) }
        .startCoroutine(Continuation(EmptyCoroutineContext) { outcome = it })
    while (true) {
        val next = parked ?: break
        parked = null
        next()
    }
    val sorted = outcome!!.getOrThrow()
    return if (sorted == "abc:2:1") "OK" else "sort $sorted"
}
"#;
    common::expect_box_same_as_kotlinc(MAIN, "Main");
}

#[test]
fn suspend_loop_statement_when_with_block_arms_over_a_map_matches_kotlinc() {
    const MAIN: &str = r#"import kotlin.coroutines.*

val pending = ArrayList<() -> Unit>()

suspend fun <T> later(value: T): T = suspendCoroutine { c -> pending.add { c.resume(value) } }

suspend fun empty(s: String): Boolean = later(s.isEmpty())

suspend fun tally(m: Map<String, String>): Int {
    var a = 0
    var b = 0
    for ((k, v) in m) {
        when (empty(v)) {
            true -> { a++ }
            false -> { b++ }
        }
    }
    return a * 10 + b
}

sealed interface Outcome {
    class Done(val size: Int) : Outcome
    class Failed(val reason: String) : Outcome
}

suspend fun attempt(host: String): Outcome =
    later(if (host.startsWith("bad")) Outcome.Failed(host) else Outcome.Done(host.length))

suspend fun provision(entries: Map<String, Map<String, String>>): Triple<Int, Int, String> {
    var succeeded = 0
    var failed = 0
    var log = ""
    for ((name, settings) in entries) {
        val host = settings["host"]?.let { it.trim() } ?: run {
            log += "missing:$name;"
            failed++
            continue
        }
        when (val result = attempt(host)) {
            is Outcome.Done -> {
                log += "done:$name:${result.size};"
                succeeded++
            }
            is Outcome.Failed -> {
                log += "failed:${result.reason};"
                failed++
            }
        }
    }
    return Triple(succeeded, failed, log)
}

fun <T> drive(block: suspend () -> T): T {
    var outcome: Result<T>? = null
    block.startCoroutine(Continuation(EmptyCoroutineContext) { outcome = it })
    while (pending.isNotEmpty()) pending.removeAt(0)()
    return outcome!!.getOrThrow()
}

fun box(): String {
    val counted = drive { tally(linkedMapOf("x" to "", "y" to "z", "w" to "")) }
    if (counted != 21) return "tally $counted"
    val (ok, bad, log) = drive {
        provision(
            linkedMapOf(
                "one" to mapOf("host" to " a.example "),
                "two" to mapOf("other" to "x"),
                "three" to mapOf("host" to "bad.example"),
            ),
        )
    }
    if (ok != 1 || bad != 2) return "provision $ok/$bad $log"
    if (log != "done:one:9;missing:two;failed:bad.example;") return "log $log"
    return "OK"
}
"#;
    common::expect_box_same_as_kotlinc(MAIN, "Main");
}
