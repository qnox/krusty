fun isClosedRange(value: Any?): Boolean = value is ClosedRange<*>

fun isFloatingPointRange(value: Any?): Boolean = value is ClosedFloatingPointRange<*>

class Unrelated

fun box(): String {
    val ints: Any = 1 until 4
    if (!isClosedRange(ints)) return "fail IntRange is ClosedRange"
    val closed = ints as ClosedRange<Int>
    if (closed.start != 1 || closed.endInclusive != 3) return "fail IntRange bounds ${closed.start}..${closed.endInclusive}"
    if (!isClosedRange(1L..5L)) return "fail LongRange is ClosedRange"
    if (!isClosedRange('a'..'c')) return "fail CharRange is ClosedRange"
    if (!isClosedRange(0.0..1.0)) return "fail Double range is ClosedRange"
    if (!isClosedRange(0.0f..1.0f)) return "fail Float range is ClosedRange"
    if (!isFloatingPointRange(0.0..1.0)) return "fail Double range is ClosedFloatingPointRange"
    if (!isFloatingPointRange(0.0f..1.0f)) return "fail Float range is ClosedFloatingPointRange"
    if (isFloatingPointRange(1..3)) return "fail IntRange is ClosedFloatingPointRange"
    if (isClosedRange(Unrelated())) return "fail unrelated object is ClosedRange"
    if (isClosedRange("text")) return "fail String is ClosedRange"
    if (isClosedRange(null)) return "fail null is ClosedRange"
    return "OK"
}
