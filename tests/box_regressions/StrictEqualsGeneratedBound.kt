// KOTLINC_VERSION: [2.5.0,)
// kotlinc 2.4 leaves the generated `equals` unbound under StrictEquals.
// LANGUAGE: +StrictEquals
import kotlin.jvm.JvmInline

data class Point(val x: Int)
OPTIONAL_JVM_INLINE_ANNOTATION value class Meters(val amount: Int)
data object Obj { val tag = "t" }
open class Base(val o: Int)
data class Sub(val s: Int) : Base(s)

fun point(a: Any): Int { if (Point(1) != a) return 0; return a.x }
fun pointEquals(a: Any): Int { if (Point(1) == a) return a.x; return 0 }
fun meters(a: Any): Int { if (Meters(2) != a) return 0; return a.amount }
fun obj(a: Any): String { if (Obj != a) return ""; return a.tag }
fun sub(a: Any): Int { if (Sub(3) != a) return 0; return a.s }
fun nullableReceiver(p: Point?, a: Any?): Int { if (p == a) return a?.x ?: -1; return 0 }
fun call(a: Any?): Int { if (Point(4).equals(a)) return a.x; return 0 }

fun box(): String {
    if (point(Point(1)) != 1) return "point"
    if (pointEquals(Point(1)) != 1) return "pointEquals"
    if (meters(Meters(2)) != 2) return "meters"
    if (obj(Obj) != "t") return "obj"
    if (sub(Sub(3)) != 3) return "sub"
    if (nullableReceiver(null, null) != -1) return "nullableReceiver null"
    if (nullableReceiver(Point(5), Point(5)) != 5) return "nullableReceiver"
    if (call(Point(4)) != 4) return "call"
    if (point("other") != 0) return "point other"
    return "OK"
}
