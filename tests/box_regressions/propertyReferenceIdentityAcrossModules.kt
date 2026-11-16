// MODULE: lib
// FILE: lib.kt
class Point(val x: Int) {
    val label: String
        get() = "p$x"
}

val Point.doubled: Int
    get() = x * 2

inline val Point.tripled: Int
    get() = x * 3

fun exportedX(): (Point) -> Int = Point::x
fun exportedLabel(): (Point) -> String = Point::label
fun exportedDoubled(): (Point) -> Int = Point::doubled
fun exportedTripled(): (Point) -> Int = Point::tripled

// MODULE: main(lib)
// FILE: user.kt
fun localY(): (Later) -> Int = Later::y

fun box(): String {
    val point = Point(21)
    val importedX = exportedX()
    val localX: (Point) -> Int = Point::x
    if (importedX != localX) return "stored property"
    if (importedX(point) != 21 || localX(point) != 21) return "stored invoke"
    val importedLabel = exportedLabel()
    val localLabel: (Point) -> String = Point::label
    if (importedLabel != localLabel) return "custom getter"
    if (importedLabel(point) != "p21") return "custom invoke"
    val importedDoubled = exportedDoubled()
    val localDoubled: (Point) -> Int = Point::doubled
    if (importedDoubled != localDoubled) return "extension getter"
    if (importedDoubled(point) != 42 || localDoubled(point) != 42) return "extension invoke"
    val importedTripled = exportedTripled()
    val localTripled: (Point) -> Int = Point::tripled
    if (importedTripled != localTripled) return "inline getter"
    if (importedTripled(point) != 63 || localTripled(point) != 63) return "inline invoke"
    val declaredLater: (Later) -> Int = Later::y
    if (localY() != declaredLater) return "file order"
    if (localY()(later(5)) != 5) return "file order invoke"
    return "OK"
}

// FILE: later.kt
class Later(val y: Int)

fun later(y: Int): Later = Later(y)
