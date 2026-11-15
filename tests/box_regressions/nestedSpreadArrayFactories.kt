// A spread of a nested array factory call takes its element type from the enclosing call's
// expected array: every `T` here is `String`, including the innermost `emptyArray()` (kotlinc 2.4.21).
fun box(): String {
    val empty: Array<String> = arrayOf(*arrayOf(*emptyArray()))
    val filled: Array<String> = arrayOf("O", *arrayOf(*arrayOf("K")), *emptyArray())
    if (empty.size != 0) return "FAIL empty: ${empty.size}"
    if (filled.size != 2) return "FAIL filled: ${filled.size}"
    return filled[0] + filled[1]
}
