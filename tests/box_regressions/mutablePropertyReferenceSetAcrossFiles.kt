// FILE: user.kt
// A mutable property reference made in one file is written through in another: the producer
// cannot know which files call `set`, so every mutable reference carries its writer.
fun box(): String {
    slotReference().set(5)
    if (slot != 5) return "FAIL top-level: $slot"
    val cell = Cell(1)
    cellReference().set(cell, 7)
    if (cell.value != 7) return "FAIL member: ${cell.value}"
    return "OK"
}

// FILE: producer.kt
import kotlin.reflect.KMutableProperty0
import kotlin.reflect.KMutableProperty1

class Cell(var value: Int)

var slot = 0

fun slotReference(): KMutableProperty0<Int> = ::slot

fun cellReference(): KMutableProperty1<Cell, Int> = Cell::value
