fun box(): String {
    val bytes = ByteArray(4); bytes.fill(7, 1, 3)
    if (bytes.toList() != listOf<Byte>(0, 7, 7, 0)) return "fail byte"
    val shorts = ShortArray(3); shorts.fill(-2)
    if (shorts.toList() != listOf<Short>(-2, -2, -2)) return "fail short"
    val chars = CharArray(3); chars.fill('x', 2, 3)
    if (chars.toList() != listOf('\u0000', '\u0000', 'x')) return "fail char"
    val ints = IntArray(3); ints.fill(5, 0, 2)
    if (ints.toList() != listOf(5, 5, 0)) return "fail int"
    val longs = LongArray(2); longs.fill(1L shl 40)
    if (longs.toList() != listOf(1L shl 40, 1L shl 40)) return "fail long"
    val floats = FloatArray(2); floats.fill(1.5f, 1, 2)
    if (floats.toList() != listOf(0f, 1.5f)) return "fail float"
    val doubles = DoubleArray(2); doubles.fill(-0.25)
    if (doubles.toList() != listOf(-0.25, -0.25)) return "fail double"
    val booleans = BooleanArray(3); booleans.fill(true, 1, 3)
    if (booleans.toList() != listOf(false, true, true)) return "fail boolean"
    val refs = arrayOfNulls<String>(3); refs.fill("k", 0, 2)
    if (refs.toList() != listOf<String?>("k", "k", null)) return "fail reference"
    return "OK"
}
