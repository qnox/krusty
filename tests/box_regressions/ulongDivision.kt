// Unsigned 64-bit division and remainder. The stdlib's `ulongDivide`/`ulongRemainder` bodies
// order the operands through `ulongCompare`, whose body calls `Long.compareTo` explicitly.

fun div(a: ULong, b: ULong): ULong = a / b
fun rem(a: ULong, b: ULong): ULong = a % b

fun box(): String {
    if (div(7uL, 2uL) != 3uL) return "fail: 7uL / 2uL"
    if (rem(7uL, 2uL) != 1uL) return "fail: 7uL % 2uL"
    // A divisor with the sign bit set: the quotient is 0 or 1 by unsigned comparison alone.
    if (div(ULong.MAX_VALUE, 0x8000000000000000uL) != 1uL) return "fail: MAX / 2^63"
    if (div(0x7FFFFFFFFFFFFFFFuL, 0x8000000000000000uL) != 0uL) return "fail: 2^63 - 1 / 2^63"
    if (rem(ULong.MAX_VALUE, 0x8000000000000000uL) != 0x7FFFFFFFFFFFFFFFuL) return "fail: MAX % 2^63"
    // A dividend above Long.MAX_VALUE with a small divisor.
    if (div(18000000000000000000uL, 7uL) != 2571428571428571428uL) return "fail: 1.8e19 / 7"
    if (rem(18000000000000000000uL, 7uL) != 4uL) return "fail: 1.8e19 % 7"
    return "OK"
}
