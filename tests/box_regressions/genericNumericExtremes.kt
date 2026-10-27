// MODULE: lib
fun <T> retainNumericBoundary(value: T): T = value

// MODULE: main(lib)
fun box(): String {
    if (retainNumericBoundary(-2147483647 - 1) != -2147483647 - 1) return "Int minimum"
    if (retainNumericBoundary(2147483647) != 2147483647) return "Int maximum"
    if (retainNumericBoundary(-9223372036854775807L - 1L) != -9223372036854775807L - 1L) return "Long minimum"
    if (retainNumericBoundary(9223372036854775807L) != 9223372036854775807L) return "Long maximum"
    if (retainNumericBoundary(-1L) != -1L) return "Long sign"
    if (retainNumericBoundary(0L) != 0L) return "Long zero"
    if (retainNumericBoundary(0) != 0) return "Int zero"
    if (retainNumericBoundary("OK") != "OK") return "String reference"
    return "OK"
}
