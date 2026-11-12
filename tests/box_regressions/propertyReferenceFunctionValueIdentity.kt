class Point(val x: Int)

fun value(transform: (Point) -> Int): Any = transform

fun box(): String {
    val first = value(Point::x)
    val second = value(Point::x)
    if (first != second) return "unequal"
    if (first != value(Point::x)) return "third"
    if (length(String::length) != length(String::length)) return "dependency"
    return "OK"
}

fun length(transform: (String) -> Int): Any = transform
