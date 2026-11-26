// KOTLINC_VERSION: [2.4.20,)
// Earlier JVM reference compilers fail during IR lowering of the consumer module.
// MODULE: lib
// FILE: Lib.kt
// LANGUAGE: +CompanionBlocks
import kotlin.reflect.KProperty

var log = ""

class Tracked<T>(private var value: T, private val tag: String) {
    init { log += "I$tag" }
    operator fun getValue(thisRef: Any?, property: KProperty<*>): T { log += "G$tag"; return value }
    operator fun setValue(thisRef: Any?, property: KProperty<*>, new: T) { log += "S$tag"; value = new }
}

open class Holder {
    companion {
        val label: String by Tracked("label", "L")
        var count: Int by Tracked(1, "C")
    }
}

// MODULE: main(lib)
// FILE: Main.kt
// LANGUAGE: +CompanionBlocks
fun box(): String {
    if (log != "") return "FAIL: initialized before first use: $log"
    if (Holder.label != "label") return "FAIL: label"
    if (Holder.label != "label") return "FAIL: label again"
    Holder.count = 5
    if (Holder.count != 5) return "FAIL: count ${Holder.count}"
    if (log != "ILICGLGLSCGC") return "FAIL: order $log"
    return "OK"
}
