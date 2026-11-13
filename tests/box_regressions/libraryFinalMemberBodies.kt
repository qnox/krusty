// WITH_STDLIB
// MODULE: lib
// FILE: library.kt
// The library body reads a member of a final ordinary class. IntRange is final and endInclusive
// returns the bound it inherits.
fun rangeEnd(start: Int, end: Int): Int = (start..end).endInclusive

// MODULE: main(lib)
// FILE: main.kt
fun box(): String {
    if (rangeEnd(1, 2) != 2) return "end"
    if (rangeEnd(0, 0) != 0) return "zero"
    if (rangeEnd(4, 9) != 9) return "high"
    return "OK"
}
