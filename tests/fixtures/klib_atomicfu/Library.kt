package fixture

import kotlinx.atomicfu.AtomicInt
import kotlinx.atomicfu.AtomicRef
import kotlinx.atomicfu.atomic

interface Handle {
    fun id(): Int
}

class Counter {
    private val total = atomic(0)
    private val last = atomic<String?>(null)

    fun add(delta: Int): Int = total.advance(delta)

    fun next(): Int = total.incrementAndGet()

    fun rename(next: String): String? = last.exchange(next)
}

class Holder {
    private val _handle = atomic<Handle?>(null)

    var handle: Handle?
        get() = _handle.value
        set(value) {
            _handle.value = value
        }
}

internal inline fun AtomicInt.advance(delta: Int): Int {
    while (true) {
        val current = value
        if (compareAndSet(current, current + delta)) return current + delta
    }
}

internal inline fun <T> AtomicRef<T>.exchange(next: T): T {
    while (true) {
        val current = value
        if (compareAndSet(current, next)) return current
    }
}
