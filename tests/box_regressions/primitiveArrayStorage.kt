// Primitive arrays store their element's own value: each classifier's literals and spreads,
// element reads and stores, `size`, iteration, and the packed elements' sign or zero extension.
fun sum(values: IntArray): Int {
    var total = 0
    for (value in values) total += value
    return total
}

fun box(): String {
    val ints = intArrayOf(1, *intArrayOf(2, 3))
    if (ints.size != 3 || sum(ints) != 6) return "ints"
    ints[0] = 10
    if (ints[0] + ints[1] + ints[2] != 15) return "int store"
    val longs = longArrayOf(1L shl 40, -1L)
    if (longs[0] + longs[1] != (1L shl 40) - 1) return "longs"
    val bytes = byteArrayOf(-1, 127)
    bytes[1]++
    if (bytes[0].toInt() != -1 || bytes[1].toInt() != -128) return "bytes ${bytes[1]}"
    val shorts = shortArrayOf(-2, 3)
    if (shorts[0] + shorts[1] != 1) return "shorts"
    val chars = charArrayOf('O', '￿')
    if (chars[1].code != 65535) return "chars"
    val booleans = booleanArrayOf(true, false)
    if (!booleans[0] || booleans[1]) return "booleans"
    val floats = floatArrayOf(0.5f, *floatArrayOf(), 1.5f)
    if (floats.size != 2 || floats[0] + floats[1] != 2.0f) return "floats"
    val doubles = doubleArrayOf(0.25, 0.75)
    if (doubles[0] + doubles[1] != 1.0) return "doubles"
    val empty = intArrayOf()
    if (empty.size != 0) return "empty"
    var text = ""
    for (char in charArrayOf('O', 'K')) text += char
    return text
}
