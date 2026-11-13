// WITH_STDLIB
// MODULE: lib
// FILE: library.kt
// The library body reads the two ordinary members. IntRange is final and endInclusive returns
// the bound it inherits; StringBuilder is final and toString is its own member.
fun rangeEnd(start: Int, end: Int): Int = (start..end).endInclusive

fun built(text: String): String = StringBuilder(text).toString()

// MODULE: main(lib)
// FILE: main.kt
fun box(): String {
    if (rangeEnd(1, 2) != 2) return "end"
    if (rangeEnd(0, 0) != 0) return "zero"
    if (rangeEnd(4, 9) != 9) return "high"
    if (built("OK") != "OK") return "text"
    if (built("") != "") return "empty"
    return "OK"
}
