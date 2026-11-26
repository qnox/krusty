// A nested call whose integer literal widens to the expected element type keeps its candidate,
// alone and among overloads: the literal's provisional `Int` is not evidence against `Long`.

fun single(values: List<Long>): String = "single" + values.sum()

fun overloaded(values: List<Long>): String = "long" + values.sum()

fun overloaded(values: String): String = "string$values"

fun inferredSingle() = single(listOf(1))

fun inferredOverloaded() = overloaded(listOf(2, 3))

fun box(): String {
    val single = single(listOf(1))
    if (single != "single1") return "single: $single"
    val overloaded = overloaded(listOf(2, 3))
    if (overloaded != "long5") return "overloaded: $overloaded"
    if (inferredSingle() != "single1") return "inferredSingle: ${inferredSingle()}"
    if (inferredOverloaded() != "long5") return "inferredOverloaded: ${inferredOverloaded()}"
    return "OK"
}
