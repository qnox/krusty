// WITH_STDLIB
// FILE: KeptAsValue.kt
import kotlin.coroutines.*

interface Sink {
    suspend fun send(item: Int)
}

interface Source {
    suspend fun consume(sink: Sink)
}

inline fun source(crossinline action: suspend Sink.() -> Unit): Source = object : Source {
    override suspend fun consume(sink: Sink) {
        action(sink)
    }
}

suspend inline fun Source.consumeEach(crossinline action: suspend (Int) -> Unit) {
    consume(object : Sink {
        override suspend fun send(item: Int) = action(item)
    })
}

suspend inline fun Source.fold(initial: Int, crossinline operation: suspend (Int, Int) -> Int): Int {
    var acc = initial
    consumeEach {
        acc = operation(acc, it)
    }
    return acc
}

inline fun Source.filter(crossinline predicate: (Int) -> Boolean): Source = source {
    consumeEach {
        if (predicate(it)) send(it)
    }
}

fun range(start: Int, count: Int): Source = source {
    for (i in start until start + count) send(i)
}

fun box(): String {
    var result = 0
    suspend { result = range(1, 4).filter { it % 2 == 0 }.fold(0) { a, b -> a + b } }
        .startCoroutine(Continuation(EmptyCoroutineContext) { it.getOrThrow() })
    return if (result == 6) "OK" else "result $result"
}
