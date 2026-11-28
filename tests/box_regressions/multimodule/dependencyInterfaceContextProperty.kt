// LANGUAGE: +ContextParameters
// MODULE: lib
// FILE: Cell.kt
package contextcells

interface Cell {
    context(scale: Int)
    var amount: Int
}

fun readInLibrary(cell: Cell): Int {
    return with(2) { cell.amount }
}
fun writeInLibrary(cell: Cell, value: Int) {
    with(2) { cell.amount = value }
}

// MODULE: main(lib)
// FILE: Main.kt
import contextcells.Cell
import contextcells.readInLibrary
import contextcells.writeInLibrary

var reads = 0
var writes = 0

class Counter : Cell {
    private var stored = 0
    context(scale: Int)
    override var amount: Int
        get() { reads++; return stored * scale }
        set(value) { writes++; stored = value / scale }
}

fun box(): String {
    val cell: Cell = Counter()
    with(3) { cell.amount = 21 }
    if (writes != 1 || reads != 0) return "setter: " + writes + "/" + reads
    if (with(3) { cell.amount } != 21 || reads != 1) return "getter at the same context"
    if (with(5) { cell.amount } != 35 || reads != 2) return "getter at another context"
    if (readInLibrary(cell) != 14 || reads != 3) return "library getter"
    writeInLibrary(cell, 50)
    if (writes != 2 || with(1) { cell.amount } != 25 || reads != 4) return "library setter"
    return "OK"
}
