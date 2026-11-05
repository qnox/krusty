// TARGET_BACKEND: JVM
// WITH_STDLIB
// WITH_ATOMICFU
// A class's Int and Long atomics become volatile fields updated through static
// java.util.concurrent field updaters; every operation becomes a call of its updater.
// FILE: fieldUpdaters.kt
package counters

import kotlinx.atomicfu.atomic

class Counter {
    private val hits = atomic(0)
    private val total = atomic(7L)
    private val start = atomic(5)

    init {
        hits.incrementAndGet()
    }

    fun increment(): Int = hits.incrementAndGet()
    fun decrement(): Int = hits.decrementAndGet()
    fun read(): Int = hits.value
    fun write(value: Int) {
        hits.value = value
    }
    fun writeLazily(value: Int) {
        hits.lazySet(value)
    }
    fun replace(expected: Int, update: Int): Boolean = hits.compareAndSet(expected, update)
    fun swap(value: Int): Int = hits.getAndSet(value)
    fun add(delta: Int): Int =
        hits.addAndGet(delta) + hits.getAndAdd(delta) + hits.getAndIncrement() + hits.getAndDecrement()
    fun addTotal(delta: Long): Long = total.addAndGet(delta) + start.value
    fun replaceTotal(expected: Long, update: Long): Boolean = total.compareAndSet(expected, update)
    fun incrementOther(other: Counter): Int = other.hits.incrementAndGet()
}

fun box(): String {
    val counter = Counter()
    if (counter.read() != 1) return "init ${counter.read()}"
    if (counter.increment() != 2) return "increment"
    if (counter.decrement() != 1) return "decrement"
    counter.write(10)
    if (counter.read() != 10) return "write"
    if (!counter.replace(10, 3)) return "replace"
    if (counter.replace(10, 4)) return "replace stale"
    if (counter.swap(4) != 3) return "swap"
    counter.writeLazily(2)
    if (counter.add(1) != 3 + 3 + 4 + 5) return "add ${counter.read()}"
    if (counter.addTotal(3) != 15L) return "total"
    if (!counter.replaceTotal(10L, 1L)) return "replace total"
    if (counter.incrementOther(Counter()) != 2) return "other"
    return "OK"
}
