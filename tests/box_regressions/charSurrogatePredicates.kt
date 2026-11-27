// Char.isHighSurrogate()/isLowSurrogate() at each edge of both halves of the surrogate block,
// through a local, a parameter and a computed receiver.

fun high(c: Char): Boolean = c.isHighSurrogate()
fun low(c: Char): Boolean = c.isLowSurrogate()

fun box(): String {
    val edges = charArrayOf('퟿', '\uD800', '\uDBFF', '\uDC00', '\uDFFF', '', 'A', '\u0000', '￿')
    val seen = StringBuilder()
    for (c in edges) {
        seen.append(if (high(c)) 'H' else '-')
        seen.append(if (low(c)) 'L' else '-')
        seen.append(' ')
    }
    if (seen.toString() != "-- H- H- -L -L -- -- -- -- ") return "FAIL " + seen
    val local = '\uDA00'
    if (!local.isHighSurrogate() || local.isLowSurrogate()) return "FAIL local"
    if (!(local + 0x0400).isLowSurrogate()) return "FAIL computed"
    val pair = "😀"
    if (!pair[0].isHighSurrogate() || !pair[1].isLowSurrogate()) return "FAIL pair"
    return "OK"
}
