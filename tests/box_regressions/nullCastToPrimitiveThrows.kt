// `null as Int` fails the cast with a Kotlin exception the program catches, not a host trap.
fun read(value: Any?): Int = value as Int

fun box(): String = try {
    read(null)
    "BAD"
} catch (failure: Throwable) {
    "OK"
}
