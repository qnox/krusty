// An enum class's primary-constructor parameters take annotations with arguments, as an ordinary
// class's do. Each annotation stays on its own parameter; it does not leak onto the entries or
// hide the enum class from the rest of the file.

annotation class Since(val version: String)

enum class Versioned(
    val first: String = "O",
    @Since("1") val second: String = "K",
    @Since("2") val third: String = "!",
) {
    DEFAULT,
    PARTIAL("P", "Q"),
    FULL("A", "B", "C");
}

fun box(): String {
    if (Versioned.DEFAULT.first + Versioned.DEFAULT.second + Versioned.DEFAULT.third != "OK!") return "FAIL default"
    if (Versioned.PARTIAL.first + Versioned.PARTIAL.second + Versioned.PARTIAL.third != "PQ!") return "FAIL partial"
    if (Versioned.FULL.first + Versioned.FULL.second + Versioned.FULL.third != "ABC") return "FAIL full"
    if (Versioned.values().size != 3) return "FAIL entries"
    return "OK"
}
