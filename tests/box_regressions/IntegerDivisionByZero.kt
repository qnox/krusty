fun zeroInt() = 0
fun zeroLong() = 0L

inline fun throwsArithmetic(block: () -> Any): String? =
    try {
        block()
        null
    } catch (e: ArithmeticException) {
        e.message
    }

fun box(): String {
    if (throwsArithmetic { 5 / zeroInt() } != "/ by zero") return "Int div"
    if (throwsArithmetic { 5 % zeroInt() } != "/ by zero") return "Int rem"
    if (throwsArithmetic { 5L / zeroLong() } != "/ by zero") return "Long div"
    if (throwsArithmetic { 5L % zeroLong() } != "/ by zero") return "Long rem"
    if (throwsArithmetic { 5.toByte() / zeroInt().toByte() } != "/ by zero") return "Byte div"
    if (7 / 3 != 2 || 7 % 3 != 1) return "nonzero divisor"
    if (Int.MIN_VALUE / -1 != Int.MIN_VALUE) return "MIN_VALUE / -1"
    if ((0.0 / 0.0).isNaN().not()) return "floating zero divisor"
    return "OK"
}
