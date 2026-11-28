// A type test against a callable nominal classifier (`KProperty1`) narrows the value to that
// classifier, not to the function type it implements. A written cast of the narrowed value then
// checks only its own target, as does a cast of a value narrowed to a function type.
import kotlin.reflect.KProperty
import kotlin.reflect.KProperty1

class Point(val x: Int)

fun read(property: KProperty<*>): Int {
    if (property !is KProperty1<*, *>) return -1
    val typed = property as KProperty1<Point, Int>
    return typed.get(Point(4))
}

fun apply(value: Any): Int {
    if (value !is Function1<*, *>) return -1
    val typed = value as (Point) -> Int
    return typed(Point(5))
}

fun box(): String {
    val read = read(Point::x)
    if (read != 4) return "read: $read"
    val applied = apply { point: Point -> point.x + 1 }
    if (applied != 6) return "apply: $applied"
    return "OK"
}
