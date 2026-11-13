// Each branch of an `if` passed to a bounded type parameter is its own lower bound on it: `T`
// solves from `Long` and `Int` within `Number`, not from the branches' join (kotlinc 2.4.21).
fun <T : Number> widen(x: T): Int = x.toInt()

fun box(): String {
    val long: Long = -10
    val int: Int = 20
    val first = widen(if (long < 0) long else int)
    val second = widen(if (long > 0) long else if (int > 0) int else 3.5)
    return if (first == -10 && second == 20) "OK" else "FAIL: $first $second"
}
