// A suspend inline function nests lambdas around a captured `var` whose type names the function's
// reified parameter, and a lambda nested in it reads that `var`. Expanding the function at a call
// that takes one of its defaults, from the body of another inline function, plans each lambda once.
import kotlin.coroutines.*

class Listener<E> {
    var completed = false
    fun complete() { completed = true }
}

class Channel {
    var delivered = 0

    // Delivers its one event at once, so nothing here ever suspends.
    fun <E : Any> subscribe(handler: (String) -> Boolean): Listener<E> {
        delivered++
        handler("event-")
        return Listener()
    }
}

class Cell<T> {
    var value: T? = null
    fun resume(result: T) { value = result }
}

fun <T> await(register: (Cell<T>) -> Unit): T {
    val cell = Cell<T>()
    register(cell)
    return cell.value!!
}

class Scope(val name: String)

suspend fun <T> scoped(block: suspend Scope.() -> T): T = Scope("scope").block()

suspend inline fun <reified E : Any> Channel.next(suffix: String = "!"): String =
    scoped {
        await { cont ->
            var listener: Listener<E>? = null
            listener = this@next.subscribe<E> { event ->
                try {
                    cont.resume(event + name + suffix)
                } finally {
                    listener?.complete()
                }
                true
            }
        }
    }

suspend inline fun <reified E : Any> Channel.nextWithDefault(): String = next<E>()

fun builder(block: suspend () -> Unit) {
    block.startCoroutine(Continuation(EmptyCoroutineContext) { it.getOrThrow() })
}

fun box(): String {
    val channel = Channel()
    var result = "fail"
    builder { result = channel.nextWithDefault<String>() }
    return if (result == "event-scope!" && channel.delivered == 1) "OK" else result
}
