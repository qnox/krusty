// A reference to an inline property is a property reference value like any other: references to
// the same property are equal however its getter is realized, and invoking one runs the getter.
class Point(val x: Int)

inline val Point.doubled: Int
    get() = x * 2

inline val answer: Int
    get() = 42

class Box(val v: Int) {
    inline val twice: Int
        get() = v * 2
}

fun extension(): (Point) -> Int = Point::doubled
fun boundExtension(point: Point): () -> Int = point::doubled
fun topLevel(): () -> Int = ::answer
fun member(): (Box) -> Int = Box::twice
fun boundMember(box: Box): () -> Int = box::twice

inline fun inlinedReference(): (Point) -> Int = Point::doubled
inline fun applied(point: Point, read: (Point) -> Int): Int = read(point)
fun inlinedOnce(): (Point) -> Int = inlinedReference()
fun inlinedTwice(): (Point) -> Int = inlinedReference()

fun box(): String {
    val point = Point(21)
    val localExtension: (Point) -> Int = Point::doubled
    if (extension() != localExtension) return "extension"
    if (extension()(point) != 42) return "extension invoke"
    val localBoundExtension: () -> Int = point::doubled
    if (boundExtension(point) != localBoundExtension) return "bound extension"
    if (boundExtension(point)() != 42) return "bound extension invoke"
    val localTopLevel: () -> Int = ::answer
    if (topLevel() != localTopLevel) return "top level"
    if (topLevel()() != 42) return "top level invoke"
    val box = Box(4)
    val localMember: (Box) -> Int = Box::twice
    if (member() != localMember) return "member"
    if (member()(box) != 8) return "member invoke"
    val localBoundMember: () -> Int = box::twice
    if (boundMember(box) != localBoundMember) return "bound member"
    if (boundMember(box)() != 8) return "bound member invoke"
    if (inlinedOnce() != inlinedTwice()) return "inlined"
    if (inlinedOnce() != localExtension) return "inlined and local"
    if (inlinedOnce()(point) != 42) return "inlined invoke"
    if (applied(point, Point::doubled) != 42) return "inline argument"
    if (applied(point, localExtension) != 42) return "inline value argument"
    return "OK"
}
