// MODULE: lib
// FILE: Outer.kt
package fixture

// A public final class whose nested classes a consumer target lowers from the library's bodies.
// The public nested class is constructed by the consumer; the private one only by its outer
// class's members, so it stays private to them.
class Outer(private val start: Int) {
    class Step(private val by: Int) {
        fun apply(value: Int): Int = value + by
    }

    private class Twice(private val step: Step) {
        fun apply(value: Int): Int = step.apply(step.apply(value))
    }

    fun next(): Int = Step(1).apply(start)

    fun skip(): Int = Twice(Step(2)).apply(start)
}

// MODULE: main(lib)
// FILE: Consumer.kt
package fixture

fun box(): String {
    val outer = Outer(40)
    if (outer.next() != 41) return "next"
    if (outer.skip() != 44) return "skip"
    if (Outer.Step(3).apply(1) != 4) return "step"
    return "OK"
}
