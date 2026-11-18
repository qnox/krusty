// A module boxes a value class its library declares, where a position holds the class as `Any`
// or as a nullable of it, and unboxes it again: a type check, a cast, `equals` and `toString` on
// the box, and a box the library itself unboxes.
// MODULE: lib
// FILE: Lib.kt
OPTIONAL_JVM_INLINE_ANNOTATION
value class Meters(val value: Int)

fun meters(value: Int) = Meters(value)

fun describe(any: Any): String = (any as Meters).value.toString()

// MODULE: main(lib)
// FILE: Main.kt
fun box(): String {
    val boxed: Any = meters(4)
    if (boxed !is Meters) return "not a Meters: $boxed"
    if ((boxed as Meters).value != 4) return "value = ${boxed.value}"
    if (describe(meters(5)) != "5") return "describe = ${describe(meters(5))}"
    if (boxed.toString() != "Meters(value=4)") return "toString = $boxed"
    val nullable: Meters? = meters(6)
    if (nullable?.value != 6) return "nullable = $nullable"
    return if (boxed == meters(4)) "OK" else "equals"
}
