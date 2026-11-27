// `String(chars)` and `String(chars, offset, length)` are deprecated with error level on Native and
// Wasm, so the conversions here are their replacements, `concatToString()` and
// `concatToString(startIndex, endIndex)`.
private fun check(name: String, text: String, length: Int, vararg units: Char): String? {
    if (text.length != length) return "$name length ${text.length}"
    if (units.size != length) return "$name expectation arity"
    for (i in 0 until length) {
        if (text[i] != units[i]) return "$name char $i"
    }
    return null
}

fun box(): String {
    val ascii = charArrayOf('a', 'b', 'c', 'd')
    val bmp = charArrayOf('x', 'é', '€', 'y')
    val pair = charArrayOf('p', '\uD83D', '\uDE00', 'q')

    check("ascii whole", ascii.concatToString(), 4, 'a', 'b', 'c', 'd')?.let { return it }
    check("ascii slice", ascii.concatToString(1, 3), 2, 'b', 'c')?.let { return it }
    check("ascii empty", ascii.concatToString(2, 2), 0)?.let { return it }

    check("bmp whole", bmp.concatToString(), 4, 'x', 'é', '€', 'y')?.let { return it }
    check("bmp slice", bmp.concatToString(1, 3), 2, 'é', '€')?.let { return it }
    if (bmp.concatToString(1, 3) != "é€") return "bmp equality"

    check("pair whole", pair.concatToString(), 4, 'p', '\uD83D', '\uDE00', 'q')?.let { return it }
    check("pair slice", pair.concatToString(1, 3), 2, '\uD83D', '\uDE00')?.let { return it }
    if (pair.concatToString(1, 3) != "😀") return "pair equality"

    // A high surrogate at the end of the slice has no low one to join, though the array holds one.
    check("lone high at the slice end", pair.concatToString(0, 2), 2, 'p', '\uD83D')?.let { return it }
    if (pair.concatToString(0, 2) == pair.concatToString(0, 3)) return "cut pair equals whole pair"

    // A low surrogate at the start of the slice has no high one before it in the slice.
    check("lone low at the slice start", pair.concatToString(2, 4), 2, '\uDE00', 'q')?.let { return it }

    check("lone low", charArrayOf('\uDE00', 'a').concatToString(), 2, '\uDE00', 'a')?.let { return it }
    check("low then high", charArrayOf('\uDE00', '\uD83D').concatToString(), 2, '\uDE00', '\uD83D')
        ?.let { return it }
    val loneHighLast = charArrayOf('a', '\uD83D')
    check("lone high last", loneHighLast.concatToString(), 2, 'a', '\uD83D')?.let { return it }
    if (loneHighLast.concatToString(1, 2) != charArrayOf('\uD83D').concatToString()) {
        return "lone high equality"
    }
    if (loneHighLast.concatToString(1, 2) == charArrayOf('\uDE00').concatToString()) {
        return "lone high equals lone low"
    }

    try {
        ascii.concatToString(2, 5)
        return "no exception past the end"
    } catch (e: IndexOutOfBoundsException) {
    }
    try {
        ascii.concatToString(-1, 2)
        return "no exception for a negative start"
    } catch (e: IndexOutOfBoundsException) {
    }
    try {
        ascii.concatToString(3, 1)
        return "no exception for a reversed range"
    } catch (e: IllegalArgumentException) {
    }

    return "OK"
}
