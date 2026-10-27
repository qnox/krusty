// MODULE: lib
fun <T> retainNumericBoundary(value: T): T = value
fun <T, R> chooseNumericBoundary(other: R, value: T): T = value

var numericArgumentOrder = 0
fun numericArgumentOrderValue(): Int = numericArgumentOrder
fun nextNumericLong(): Long {
    numericArgumentOrder = numericArgumentOrder * 10 + 1
    return -9223372036854775807L - 1L
}
fun nextNumericText(): String {
    numericArgumentOrder = numericArgumentOrder * 10 + 2
    return "unused"
}

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
    if (chooseNumericBoundary(value = -2147483647 - 1, other = "ignored") != -2147483647 - 1) return "named Int argument"
    if (chooseNumericBoundary(value = 9223372036854775807L, other = "ignored") != 9223372036854775807L) return "named Long argument"
    if (chooseNumericBoundary(value = nextNumericLong(), other = nextNumericText()) != -9223372036854775807L - 1L) return "spilled Long argument"
    if (numericArgumentOrderValue() != 12) return "named argument evaluation order"
    return "OK"
}
