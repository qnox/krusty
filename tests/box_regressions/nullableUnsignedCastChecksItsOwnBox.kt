// `as UInt?` on a reference checks it against the `UInt` box, never the `Int` one: a null and a
// `UInt` pass and keep their number, and an `Int` box of the same bits fails the cast with a
// Kotlin exception the program catches.
fun read(value: Any?): UInt? = value as UInt?

fun box(): String {
    if (read(null) != null) return "fail null"
    if (read(7u) != 7u) return "fail UInt"
    return try {
        read(7)
        "BAD"
    } catch (failure: ClassCastException) {
        "OK"
    }
}
