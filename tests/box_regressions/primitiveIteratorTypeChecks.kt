fun box(): String {
    val booleans: Any = booleanArrayOf(true).iterator()
    val bytes: Any = byteArrayOf(1).iterator()
    val shorts: Any = shortArrayOf(1).iterator()
    val chars: Any = charArrayOf('a').iterator()
    val ints: Any = intArrayOf(1).iterator()
    val longs: Any = longArrayOf(1L).iterator()
    val floats: Any = floatArrayOf(1f).iterator()
    val doubles: Any = doubleArrayOf(1.0).iterator()

    if (booleans !is BooleanIterator) return "FAIL: boolean iterator"
    if (bytes !is ByteIterator) return "FAIL: byte iterator"
    if (shorts !is ShortIterator) return "FAIL: short iterator"
    if (chars !is CharIterator) return "FAIL: char iterator"
    if (ints !is IntIterator) return "FAIL: int iterator"
    if (longs !is LongIterator) return "FAIL: long iterator"
    if (floats !is FloatIterator) return "FAIL: float iterator"
    if (doubles !is DoubleIterator) return "FAIL: double iterator"

    if (booleans is ByteIterator) return "FAIL: boolean iterator is a byte iterator"
    if (ints is LongIterator) return "FAIL: int iterator is a long iterator"
    if (chars is IntIterator) return "FAIL: char iterator is an int iterator"
    val text: Any = "text"
    if (text is IntIterator) return "FAIL: a String is an iterator"
    if ((longs as? LongIterator) == null) return "FAIL: long safe cast"
    if ((longs as? IntIterator) != null) return "FAIL: long iterator safe-cast to int iterator"
    return "OK"
}
