// Two function values of one property reference are equal and hash alike, through Any.
class Point(val x: Int)

fun value(transform: (Point) -> Int): Any = transform

fun box(): String {
    val first = value(Point::x)
    val second = value(Point::x)
    if (first != second) return "unequal"
    return if (first.hashCode() == second.hashCode()) "OK" else "hash"
}
