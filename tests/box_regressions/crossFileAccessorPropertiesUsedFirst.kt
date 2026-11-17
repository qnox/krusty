// LANGUAGE: +CompanionBlocks
// crossFileAccessorProperties.kt with the using file first: another file's properties realized
// through their accessors are reached the same way whichever file is lowered first.
// FILE: main.kt
fun box(): String {
    if (backed != 2) return "initial getter: $backed"
    backed = 3
    if (backed != 4) return "setter/getter: $backed"
    if (computed != 5) return "computed: $computed"
    C.blockForwarded = 6
    if (C.blockForwarded != 6 || C.blockComputed != 60) return "block: ${C.blockComputed}"
    if (W.one != 1) return "value class: ${W.one}"
    return "OK"
}

// FILE: declarations.kt
private var cell = 4

var backed = 1
    get() = field + 1
    set(value) {
        field = value
    }
val computed get() = cell + 1

class C {
    companion {
        val blockComputed get() = cell * 10
        var blockForwarded: Int
            get() = cell
            set(value) {
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
