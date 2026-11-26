// KOTLINC_VERSION: [2.4.20,)
// Earlier JVM reference compilers fail during IR lowering of the consumer module.
// MODULE: lib
// FILE: Lib.kt
// LANGUAGE: +CompanionBlocks
var log = ""

class Cell<T>(private var value: T, private val onSet: (T) -> Unit) {
    operator fun getValue(thisRef: Any?, property: kotlin.reflect.KProperty<*>): T = value
    operator fun setValue(thisRef: Any?, property: kotlin.reflect.KProperty<*>, new: T) {
        value = new
        onSet(new)
    }
}

open class Holder {
    companion {
        var count: Int by Cell(1) { log += "C$it" }
    }
}

// MODULE: main(lib)
// FILE: Main.kt
// LANGUAGE: +CompanionBlocks
fun box(): String {
    Holder.count = 5
    if (Holder.count != 5) return "FAIL: count ${Holder.count}"
    if (log != "C5") return "FAIL: log $log"
    return "OK"
}
