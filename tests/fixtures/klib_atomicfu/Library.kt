package fixture

import kotlinx.atomicfu.AtomicInt
import kotlinx.atomicfu.AtomicRef
import kotlinx.atomicfu.atomic

class Counter {
    private val total = atomic(0)
    private val last = atomic<String?>(null)

    fun add(delta: Int): Int = total.advance(delta)

    fun next(): Int = total.incrementAndGet()

    fun rename(next: String): String? = last.exchange(next)
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
