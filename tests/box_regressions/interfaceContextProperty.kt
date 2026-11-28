// LANGUAGE: +ContextParameters
interface Cell {
    context(scale: Int)
    var amount: Int
    context(left: Int, right: Long)
    val sum: Int
}

var reads = 0
var writes = 0

class Counter : Cell {
    private var stored = 0
    context(scale: Int)
    override var amount: Int
        get() { reads++; return stored * scale }
        set(value) { writes++; stored = value / scale }
    context(left: Int, right: Long)
    override val sum: Int
        get() = left + right.toInt() + stored
}

fun readThrough(cell: Cell): Int {
    return with(2) { cell.amount }
}
fun writeThrough(cell: Cell, value: Int) {
    with(2) { cell.amount = value }
}

fun box(): String {
    val cell: Cell = Counter()
    with(3) { cell.amount = 21 }
    if (writes != 1 || reads != 0) return "setter: " + writes + "/" + reads
    if (with(3) { cell.amount } != 21 || reads != 1) return "getter at the same context"
    if (with(5) { cell.amount } != 35 || reads != 2) return "getter at another context"
    if (readThrough(cell) != 14 || reads != 3) return "getter through a function"
    writeThrough(cell, 50)
    if (writes != 2 || with(1) { cell.amount } != 25) return "setter through a function"
    val sum = with(10) { with(20L) { cell.sum } }
    if (sum != 55) return "two context parameters"
    return "OK"
}
