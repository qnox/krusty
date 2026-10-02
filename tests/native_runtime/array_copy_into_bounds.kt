// Kotlin's answer for the one copy in `array_copy_into_bounds.c` that a Kotlin program can ask
// for: a spread that fits, landing after the arguments before it.
fun f(vararg xs: Int) = xs.toList()

fun box(): String = buildString {
    appendLine(f(0, 0, 0, *intArrayOf(9, 9)))
}
