// `as Int?` on a reference checks it against the `Int` box: a null and an `Int` pass, and a `Long`
// box fails the cast with a Kotlin exception the program catches, not an unchecked host value.
fun read(value: Any?): Int? = value as Int?

fun box(): String {
    if (read(null) != null) return "fail null"
    if (read(42) != 42) return "fail Int"
    return try {
        read(1L)
        "BAD"
    } catch (failure: Throwable) {
        "OK"
    }
}
