// WITH_STDLIB
// WITH_ATOMICFU
// A class's Boolean atomics and every operation on them. On the JVM, kotlinc's atomicfu plugin
// stores each one in a volatile Int field (1 for true), converting every value it writes and reads.
// FILE: booleanFieldUpdaters.kt
package flags

import kotlinx.atomicfu.atomic

class Flags(start: Boolean) {
    private val ready = atomic(false)
    private val done = atomic(true)
    private val on = atomic(start)

    fun flip(b: Boolean): Boolean {
        on.value = b
        ready.lazySet(true)
        val previous = on.getAndSet(!b)
        val swapped = done.compareAndSet(true, false)
        return ready.compareAndSet(previous, b) && swapped && !on.value
    }

    fun state(): String = "${ready.value} ${done.value} ${on.value}"
}

fun box(): String {
    val flags = Flags(false)
    if (flags.state() != "false true false") return "fail: initial ${flags.state()}"
    if (!flags.flip(true)) return "fail: flip"
    if (flags.state() != "true false false") return "fail: flipped ${flags.state()}"
    if (Flags(true).flip(false)) return "fail: second flip"
    return "OK"
}
