// An integer literal argument of a nested generic call has an integer literal type, so the call's
// `Int` reading is not evidence against an enclosing `Long` expectation, whether the literal is
// written `1` or `-1`.
fun <T> one(element: T): List<T> = listOf(element)

fun pick(xs: List<Long>): String = "longs"
fun pick(xs: Any): String = "any"

fun first(xs: List<Long>): Long = xs[0]

fun box(): String {
    val plain = pick(one(1))
    val negative = pick(one(-1))
    val single = first(one(-2))
    return if (plain == "longs" && negative == "longs" && single == -2L) "OK"
    else "FAIL: $plain $negative $single"
}
