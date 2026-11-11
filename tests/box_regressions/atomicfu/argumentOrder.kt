// WITH_STDLIB
// WITH_ATOMICFU
// Arguments of an atomic operation run before the operation reads or writes the field.
// compareAndSet evaluates its update even when the comparison fails, and addAndGet / getAndSet
// observe a mutation that argument made to the same atomic.
// FILE: argumentOrder.kt
package ordering

import kotlinx.atomicfu.atomic

var trace = ""

fun mark(label: String, value: Int): Int {
    trace = trace + label
    return value
}

fun markLong(label: String, value: Long): Long {
    trace = trace + label
    return value
}

class Cell {
    private val n = atomic(1)
    private val wide = atomic(1L)

    fun self(): Cell {
        trace = trace + "r"
        return this
    }

    fun casMiss(): Boolean = self().n.compareAndSet(0, mark("u", 9))

    fun casHit(): Boolean = n.compareAndSet(1, mark("v", 4))

    fun bump(): Int {
        n.incrementAndGet()
        return 1
    }

    fun bumpThenAdd(): Int = n.addAndGet(mark("a", bump()))

    fun bumpThenSwap(): Int = n.getAndSet(mark("s", bump()))

    fun casMissLong(): Boolean = wide.compareAndSet(0L, markLong("w", 9L))

    fun bumpWide(): Long {
        wide.incrementAndGet()
        return 1L
    }

    fun bumpThenAddWide(): Long = wide.addAndGet(markLong("b", bumpWide()))

    fun read(): Int = n.value

    fun readWide(): Long = wide.value
}

fun box(): String {
    val cell = Cell()
    if (cell.casMiss()) return "miss applied"
    if (trace != "ru") return "miss trace $trace"
    if (cell.read() != 1) return "miss value ${cell.read()}"

    trace = ""
    if (!cell.casHit()) return "hit rejected"
    if (trace != "v") return "hit trace $trace"
    if (cell.read() != 4) return "hit value ${cell.read()}"

    trace = ""
    val added = cell.bumpThenAdd()
    if (trace != "a") return "add trace $trace"
    if (added != 6) return "add $added"
    if (cell.read() != 6) return "add value ${cell.read()}"

    trace = ""
    val previous = cell.bumpThenSwap()
    if (trace != "s") return "swap trace $trace"
    if (previous != 7) return "swap $previous"
    if (cell.read() != 1) return "swap value ${cell.read()}"

    trace = ""
    if (cell.casMissLong()) return "wide miss applied"
    if (trace != "w") return "wide trace $trace"
    if (cell.readWide() != 1L) return "wide miss ${cell.readWide()}"

    trace = ""
    val wideAdded = cell.bumpThenAddWide()
    if (trace != "b") return "wide add trace $trace"
    if (wideAdded != 3L) return "wide add $wideAdded"
    if (cell.readWide() != 3L) return "wide value ${cell.readWide()}"
    return "OK"
}
