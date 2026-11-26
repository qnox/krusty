fun isClosedRange(value: Any?): Boolean = value is ClosedRange<*>

fun box(): String {
    val range = "a".."c"
    if (!isClosedRange(range)) return "fail range"
    if (!isClosedRange("a".."c")) return "fail literal"
    if (isClosedRange("a")) return "fail String"
    if (isClosedRange(null)) return "fail null"
    return "OK"
}
