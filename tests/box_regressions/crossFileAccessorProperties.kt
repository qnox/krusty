// LANGUAGE: +CompanionBlocks
// Receiver-less properties realized only through their accessors, read and written from another
// file: computed package properties, and computed `companion { }` block properties of a class and
// of a value class.
// FILE: declarations.kt
private var cell = 1
private var log = ""

val computed get() = cell + 1
var forwarded: Int
    get() = cell
    set(value) {
        log += "set $value;"
        cell = value
    }

class C {
    companion {
        val blockComputed get() = cell * 10
        var blockForwarded: Int
            get() = cell
            set(value) {
                log += "block $value;"
                cell = value
            }
    }
}

OPTIONAL_JVM_INLINE_ANNOTATION
value class W(val value: Int) {
    companion {
        val one get() = 1
    }
}

fun log() = log

// FILE: main.kt
fun box(): String {
    if (computed != 2) return "computed: $computed"
    forwarded = 3
    if (forwarded != 3 || computed != 4) return "forwarded: $forwarded"
    forwarded += 1
    if (forwarded != 4) return "compound: $forwarded"
    if (C.blockComputed != 40) return "block computed: ${C.blockComputed}"
    C.blockForwarded = 5
    C.blockForwarded++
    if (C.blockForwarded != 6 || forwarded != 6) return "block forwarded: ${C.blockForwarded}"
    if (W.one + W(2).value != 3) return "value class: ${W.one}"
    if (log() != "set 3;set 4;block 5;block 6;") return "log: ${log()}"
    return "OK"
}
