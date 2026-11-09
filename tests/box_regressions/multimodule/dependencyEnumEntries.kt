// MODULE: library
// FILE: library.kt
package dependencyenum

enum class Level { LOW, HIGH }

fun high(): Level = Level.HIGH

// MODULE: main(library)
// FILE: main.kt
import dependencyenum.Level
import dependencyenum.high

fun box(): String {
    val read = Level.HIGH
    if (read !== high()) return "fail: identity"
    if (read === Level.LOW) return "fail: constants alias"
    if (read.ordinal != 1) return "fail: ordinal ${read.ordinal}"
    if (Level.LOW.ordinal != 0) return "fail: first ordinal ${Level.LOW.ordinal}"
    return if (read.name == "HIGH") "OK" else "fail: name ${read.name}"
}
