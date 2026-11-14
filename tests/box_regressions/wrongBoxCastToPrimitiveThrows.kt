// A `Long` box cast to `Int` fails the cast with a Kotlin exception the program catches, not a
// host trap, although the two boxes have different layouts.
fun read(value: Any?): Int = value as Int

fun box(): String = try {
    read(1L)
    "BAD"
} catch (failure: Throwable) {
    "OK"
}
