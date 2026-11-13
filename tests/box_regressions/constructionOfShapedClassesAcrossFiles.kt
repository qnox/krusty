// WITH_STDLIB
// FILE: Early.kt
// Constructs inner, value and generic classes a later file declares.
fun early(): String {
    val outer = Outer("o")
    val inner = outer.Inner(2)
    val secondary = outer.Inner("s")
    val meters = Meters(5)
    val box = Box("b")
    val pair = Pair2(1, "two")
    val distinct = if ((box as Any) !== (pair as Any)) "g" else "-"
    return inner.describe() + secondary.describe() + meters.double().value + distinct
}

// FILE: Shapes.kt
class Outer(val name: String) {
    inner class Inner(val n: Int) {
        constructor(tag: String) : this(tag.length)

        fun describe(): String = name + n
    }
}

OPTIONAL_JVM_INLINE_ANNOTATION
value class Meters(val value: Int) {
    fun double(): Meters = Meters(value * 2)
}

class Box<T>(val value: T)

class Pair2<A, B>(val first: A, val second: B)

// FILE: Main.kt
fun box(): String {
    val result = early()
    if (result != "o2o110g") return "Fail: " + result
    if (Outer("x").Inner(1).describe() != "x1") return "Fail late"
    return "OK"
}
