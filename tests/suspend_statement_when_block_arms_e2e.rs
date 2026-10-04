//! A statement `when` whose arms are blocks, in a suspending loop the state-machine flattener
//! splits. Normalizing an arm block for the flattener demotes its trailing value (`{ a++ }`) to a
//! statement, so the arm no longer yields the `when`'s checked result. A deeply exhaustive `when`
//! without `else` still recorded that result (`Int`), so emission kept a joined value no arm
//! produced and popped an empty stack: the frame computation declined with `Unsteppable` and the
//! compile panicked. A private corpus hit it in a `for ((name, cfg) in map)` loop over a sealed
//! result. Iterating a `Map` with destructuring (an inline `component1`/`component2` splice) is what
//! sends the loop through the flattener rather than the loop transformer.
use super::common;

#[test]
fn suspend_loop_statement_when_with_block_arms_runs() {
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
    common::expect_box_ok_with_stdlib(MAIN, "Main");
}
