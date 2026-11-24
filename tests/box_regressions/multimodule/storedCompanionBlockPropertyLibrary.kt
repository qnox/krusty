// KOTLINC_VERSION: [2.4.20,)
// Earlier JVM reference compilers fail during IR lowering of the consumer module.
// MODULE: lib
// FILE: Lib.kt
// LANGUAGE: +CompanionBlocks
var log = ""

open class Holder {
    companion {
        var count: Int = run { log += "H"; 1 }
        val label: String = run { log += "L"; "label" }
        internal var hidden: String = "hidden"
        fun bump(): Int = ++count
    }
}

// MODULE: main(lib)
// FILE: Main.kt
// LANGUAGE: +CompanionBlocks
fun box(): String {
    if (log != "") return "FAIL: initialized before first use: $log"
    Holder.count = 10
    if (Holder.bump() != 11) return "FAIL: bump"
    if (Holder.count != 11) return "FAIL: count ${Holder.count}"
    if (Holder.label != "label") return "FAIL: label"
    if (log != "HL") return "FAIL: order $log"
    return "OK"
}
