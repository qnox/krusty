// FILE: ConstructorLambdas.kt
// A regenerated object's constructor inlines the lambdas it invokes, whether it reads one only from
// its parameter or also keeps it in a field. The copy takes the captured values of the lambdas it
// keeps in fields first, in the order it stores them, then those of the others.

inline fun counted(crossinline start: () -> Int, crossinline step: (Int) -> Int) =
    object {
        var value = start()

        init {
            value = step(value)
        }

        override fun toString(): String {
            value = step(value)
            return value.toString()
        }
    }

inline fun stepFirst(crossinline step: (Int) -> Int, crossinline start: () -> Int) =
    object {
        var value = start()

        init {
            value = step(value)
        }

        override fun toString(): String {
            value = step(value)
            return value.toString()
        }
    }

inline fun twice(crossinline first: () -> Int, crossinline second: () -> Int) =
    object {
        val a = first()
        val b = second()

        override fun toString(): String = "${first() + a + b + second()}"
    }

fun box(): String {
    val base = 40
    val increment = 1
    val counted = counted({ base }, { it + increment }).toString()
    if (counted != "42") return "counted: $counted"
    val stepFirst = stepFirst({ it + increment }, { base }).toString()
    if (stepFirst != "42") return "stepFirst: $stepFirst"
    val twice = twice({ base }, { increment }).toString()
    if (twice != "82") return "twice: $twice"
    return "OK"
}
