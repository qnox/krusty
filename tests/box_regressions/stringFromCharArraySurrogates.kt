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

    check("ascii whole", String(ascii), 4, 'a', 'b', 'c', 'd')?.let { return it }
    check("ascii concat", ascii.concatToString(), 4, 'a', 'b', 'c', 'd')?.let { return it }
    check("ascii slice", String(ascii, 1, 2), 2, 'b', 'c')?.let { return it }
    check("ascii concat slice", ascii.concatToString(1, 3), 2, 'b', 'c')?.let { return it }
    check("ascii empty", String(ascii, 2, 0), 0)?.let { return it }

    check("bmp whole", String(bmp), 4, 'x', 'é', '€', 'y')?.let { return it }
    check("bmp concat", bmp.concatToString(), 4, 'x', 'é', '€', 'y')?.let { return it }
    check("bmp slice", String(bmp, 1, 2), 2, 'é', '€')?.let { return it }
    if (String(bmp, 1, 2) != "é€") return "bmp equality"

    check("pair whole", String(pair), 4, 'p', '\uD83D', '\uDE00', 'q')?.let { return it }
    check("pair concat", pair.concatToString(), 4, 'p', '\uD83D', '\uDE00', 'q')?.let { return it }
    check("pair slice", String(pair, 1, 2), 2, '\uD83D', '\uDE00')?.let { return it }
    if (String(pair, 1, 2) != "😀") return "pair equality"

    // A high surrogate at the end of the slice has no low one to join, though the array holds one.
    check("lone high at the slice end", String(pair, 0, 2), 2, 'p', '\uD83D')?.let { return it }
    check("lone high concat", pair.concatToString(0, 2), 2, 'p', '\uD83D')?.let { return it }
    if (String(pair, 0, 2) == String(pair, 0, 3)) return "cut pair equals whole pair"

    // A low surrogate at the start of the slice has no high one before it in the slice.
    check("lone low at the slice start", String(pair, 2, 2), 2, '\uDE00', 'q')?.let { return it }
    check("lone low concat", pair.concatToString(2, 4), 2, '\uDE00', 'q')?.let { return it }

    val loneLow = charArrayOf('\uDE00', 'a')
    check("lone low", String(loneLow), 2, '\uDE00', 'a')?.let { return it }
    val reversed = charArrayOf('\uDE00', '\uD83D')
    check("low then high", String(reversed), 2, '\uDE00', '\uD83D')?.let { return it }
    val loneHighLast = charArrayOf('a', '\uD83D')
    check("lone high last", String(loneHighLast), 2, 'a', '\uD83D')?.let { return it }
    if (String(loneHighLast, 1, 1) != String(charArrayOf('\uD83D'))) return "lone high equality"
    if (String(loneHighLast, 1, 1) == String(charArrayOf('\uDE00'))) return "lone high equals lone low"

    return "OK"
}
