// A function reference is a `KCallable` whose `name` is the referenced declaration's name. A
// program reads it through the reference's own type or through a `KCallable` it was passed as, for
// a top-level, member, extension, bound, local, constructor, and library function alike.
import kotlin.reflect.KCallable
import kotlin.reflect.KFunction0
import kotlin.reflect.KFunction1

class Point(val x: Int, val y: Int) {
    fun sum(): Int = x + y
}

fun Point.render(): String = "P:$x:$y"

fun top(value: Int): Int = value

fun nameOf(callable: KCallable<*>): String = callable.name

fun box(): String {
    val bound: KFunction0<String> = Point(1, 2)::render
    if (bound() != "P:1:2") return "FAIL bound result: ${bound()}"
    if (bound.name != "render") return "FAIL bound: ${bound.name}"

    val topLevel: KFunction1<Int, Int> = ::top
    if (topLevel(7) != 7) return "FAIL top result"
    if (topLevel.name != "top") return "FAIL top: ${topLevel.name}"
    if (nameOf(::top) != "top") return "FAIL top through KCallable: ${nameOf(::top)}"

    if (nameOf(Point::sum) != "sum") return "FAIL member: ${nameOf(Point::sum)}"
    if (nameOf(Point(3, 4)::sum) != "sum") return "FAIL bound member: ${nameOf(Point(3, 4)::sum)}"
    if (nameOf(Point::render) != "render") return "FAIL extension: ${nameOf(Point::render)}"
    if (nameOf(::Point) != "<init>") return "FAIL constructor: ${nameOf(::Point)}"

    fun local(): Int = 1
    if (nameOf(::local) != "local") return "FAIL local: ${nameOf(::local)}"

    val coerce: (Int, Int) -> Int = Int::coerceAtLeast
    if (coerce(1, 3) != 3) return "FAIL library result"
    if (nameOf(Int::coerceAtLeast) != "coerceAtLeast") return "FAIL library: ${nameOf(Int::coerceAtLeast)}"
    return "OK"
}
