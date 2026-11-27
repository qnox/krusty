// MODULE: lib
// kotlinc lets a file that suppresses INVISIBLE_REFERENCE read a dependency's internal top-level
// properties by their qualified names, whether they have a backing field or only a getter, like
// its internal functions and constants.
package lib

internal val FLAG: Boolean = true
internal val NAME: String get() = "N"
internal const val COUNT = 2
internal fun helper(): String = "K"

// MODULE: main(lib)
@file:Suppress("INVISIBLE_REFERENCE", "INVISIBLE_MEMBER")

fun box(): String {
    if (!lib.FLAG) return "field"
    if (lib.NAME != "N") return "getter"
    if (lib.COUNT != 2) return "const"
    return "O" + lib.helper()
}
