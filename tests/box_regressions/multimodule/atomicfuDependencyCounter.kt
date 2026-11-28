// WITH_STDLIB
// WITH_ATOMICFU
// A dependency class's Int and Long atomics, updated through its members from another module.
// Every module compiles against the atomicfu runtime the directive adds.
// MODULE: lib
// FILE: Counter.kt
package lib

import kotlinx.atomicfu.atomic

class Counter {
    private val hits = atomic(0)
    private val total = atomic(10L)

    fun hit(): Int = hits.incrementAndGet()

    fun add(delta: Long): Long = total.addAndGet(delta)

    fun replace(expected: Int, update: Int): Boolean = hits.compareAndSet(expected, update)

    fun hits(): Int = hits.value
}

// MODULE: main(lib)
// FILE: main.kt
import lib.Counter

fun box(): String {
    val counter = Counter()
    if (counter.hit() != 1) return "fail: first hit"
    if (counter.hit() != 2) return "fail: second hit"
    if (counter.add(5L) != 15L) return "fail: add"
    if (!counter.replace(2, 7)) return "fail: replace"
    if (counter.replace(2, 9)) return "fail: stale replace"
    if (counter.hits() != 7) return "fail: hits ${counter.hits()}"
    return "OK"
}
