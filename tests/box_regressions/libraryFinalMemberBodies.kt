// WITH_STDLIB
// MODULE: lib
// FILE: library.kt
// The library body reads the two ordinary members. IntRange is final and endInclusive returns
// the bound it inherits; StringBuilder is final and toString is its own member.
fun rangeEnd(start: Int, end: Int): Int = (start..end).endInclusive

fun rangeContains(start: Int, end: Int, value: Int): Boolean = (start..end).contains(value)

fun charRangeContains(start: Char, end: Char, value: Char): Boolean = (start..end).contains(value)

fun longRangeEnd(start: Long, end: Long): Long = (start..end).endInclusive

fun built(text: String): String = StringBuilder(text).toString()

// MODULE: main(lib)
// FILE: main.kt
fun box(): String {
    if (rangeEnd(1, 2) != 2) return "end"
    if (rangeEnd(0, 0) != 0) return "zero"
    if (rangeEnd(4, 9) != 9) return "high"
    if (!rangeContains(1, 3, 2)) return "contains"
    if (rangeContains(1, 3, 4)) return "outside"
    if (rangeContains(3, 1, 2)) return "empty range"
    if (!charRangeContains('a', 'c', 'b')) return "char contains"
    if (charRangeContains('a', 'c', 'd')) return "char outside"
    if (longRangeEnd(1L, 9L) != 9L) return "long end"
    if (built("OK") != "OK") return "text"
    if (built("") != "") return "empty"
    return "OK"
}
