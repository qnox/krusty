// MODULE: lib
// FILE: Library.kt
package fixture

// File-private top-level properties whose initializers run code. A consumer target lowers the
// library's bodies, which read them through their private accessors. The file is initialized the
// first time one of its declarations is used: every initializer runs once, in declaration order,
// and an initializer that reads a later property of the file sees that property's default.
private var steps = 0
private val first = step()
private val table = intArrayOf(3, 1, 4, 1, 5)
private val early = peekLate()
private val late = step() + 10
private var cursor: Int = table.size

private fun peekLate(): Int = late

private fun step(): Int {
    steps += 1
    return steps
}

fun tableAt(index: Int): Int = table[index]

fun order(): Int = first * 100 + late

fun earlyRead(): Int = early

fun initializationSteps(): Int = steps

fun advance(): Int {
    cursor -= 1
    return cursor
}

// MODULE: main(lib)
// FILE: Consumer.kt
package fixture

fun box(): String {
    if (tableAt(2) != 4) return "table"
    if (initializationSteps() != 2) return "initialized once: ${initializationSteps()}"
    if (order() != 112) return "order: ${order()}"
    if (earlyRead() != 0) return "a forward read sees the default"
    if (advance() != 4) return "first advance"
    if (advance() != 3) return "second advance"
    if (initializationSteps() != 2) return "initialized again"
    return "OK"
}
