// A `suspend inline` function that invokes an inline suspend parameter returning `Unit` marks the
// result of the invoke in the body it keeps for inlining callers (`InlineMarker.mark(7)`), and every one but the first is a suspension point of its own.
import kotlin.coroutines.*

suspend inline fun run(block: suspend () -> Unit) {
    block()
}

suspend inline fun runTwice(block: suspend () -> Unit) {
    block()
    block()
}

suspend inline fun runThrice(block: suspend () -> Unit) {
    block()
    block()
    block()
}

fun box(): String = "OK"
