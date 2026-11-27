class Bound(val value: Int) {
    operator fun rangeTo(other: Bound): Span = Span(value, other.value)
}

class Span(val low: Int, val high: Int) {
    operator fun <T : Comparable<T>> contains(element: T): Boolean = element is Int && element >= low && element <= high
}

fun box(): String {
    val inside = 5 in Bound(1)..Bound(9)
    val outside = 11 in Bound(1)..Bound(9)
    return if (inside && !outside) "OK" else "fail"
}
