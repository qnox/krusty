annotation class Inner(
    val fallback: Int = 9,
    val first: Int,
    val second: Int,
    vararg val rest: Int
)

annotation class Outer(val value: Inner = Inner(second = 2, first = 1))

fun make(): Outer = Outer()
