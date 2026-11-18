fun box(): String {
    val ints: Any = 1 until 4
    if (ints !is ClosedRange<*>) return "fail IntRange is ClosedRange"
    val closed = ints as ClosedRange<Int>
    if (closed.start != 1 || closed.endInclusive != 3) return "fail IntRange bounds ${closed.start}..${closed.endInclusive}"
    val longs: Any = 1L..5L
    if (longs !is ClosedRange<*>) return "fail LongRange is ClosedRange"
    val chars: Any = 'a'..'c'
    if (chars !is ClosedRange<*>) return "fail CharRange is ClosedRange"
    val text: Any = "text"
    if (text is ClosedRange<*>) return "fail String is ClosedRange"
    return "OK"
}
