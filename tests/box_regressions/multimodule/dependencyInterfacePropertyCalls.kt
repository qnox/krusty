// MODULE: lib
// FILE: Cell.kt
package propertycalls

interface Cell {
    var count: Int
    val label: String
    fun text(): String
}

fun readInLibrary(cell: Cell): Int = cell.count
fun writeInLibrary(cell: Cell, value: Int) { cell.count = value }

// MODULE: main(lib)
// FILE: Main.kt
import propertycalls.Cell
import propertycalls.readInLibrary
import propertycalls.writeInLibrary

var events = ""
var reads = 0
var writes = 0

class Counter : Cell {
    private var stored = 1
    override var count: Int
        get() { reads++; return stored }
        set(value) { writes++; stored = value }
    override val label: String get() = "cell"
    override fun text(): String = "method"
}

fun receiver(cell: Cell): Cell { events += "R"; return cell }
fun value(): Int { events += "V"; return 7 }
fun failingReceiver(): Cell { events += "R"; throw IllegalStateException("receiver") }
fun failingValue(): Int { events += "V"; throw IllegalStateException("value") }

fun box(): String {
    val cell: Cell = Counter()
    receiver(cell).count = value()
    if (events != "RV" || writes != 1) return "setter order: " + events
    if (receiver(cell).count != 7 || reads != 1 || events != "RVR") return "getter once"
    if (cell.label != "cell" || cell.text() != "method") return "member roles"
    if (readInLibrary(cell) != 7 || reads != 2) return "library getter"
    writeInLibrary(cell, 9)
    if (cell.count != 9 || reads != 3 || writes != 2) return "library setter"

    events = ""
    try {
        failingReceiver().count = value()
        return "receiver did not throw"
    } catch (failure: IllegalStateException) {
        if (failure.message != "receiver" || events != "R" || writes != 2) return "receiver exception"
    }
    events = ""
    try {
        receiver(cell).count = failingValue()
        return "value did not throw"
    } catch (failure: IllegalStateException) {
        if (failure.message != "value" || events != "RV" || writes != 2) return "value exception"
    }
    return "OK"
}
