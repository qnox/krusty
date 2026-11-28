// MODULE: lib
// FILE: Slot.kt
package contextsetter

open class Slot {
    var stored = 0
    var seen = ""

    context(prefix: String, scale: Long)
    var scaled: Int
        get() { seen += prefix; return (stored * scale).toInt() }
        set(value) { seen += prefix; stored = (value / scale).toInt() }
}

// MODULE: main(lib)
// FILE: Main.kt
import contextsetter.Slot

fun box(): String {
    val slot: Slot = Slot()
    with("p") {
        with(2L) {
            slot.scaled = 10
            if (slot.stored != 5) return "setter stored " + slot.stored
            if (slot.scaled != 10) return "getter " + slot.scaled
        }
    }
    if (slot.seen != "pp") return "contexts " + slot.seen
    return "OK"
}
