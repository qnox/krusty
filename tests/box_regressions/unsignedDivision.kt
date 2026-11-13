// Unsigned division and remainder of the 32-bit and narrower unsigned numbers. Each reaches the
// stdlib's `uintDivide`/`uintRemainder`, whose bodies read the value a `UInt` wraps.

fun div(a: UInt, b: UInt): UInt = a / b
fun rem(a: UInt, b: UInt): UInt = a % b
fun div(a: UByte, b: UByte): UInt = a / b
fun rem(a: UShort, b: UShort): UInt = a % b
fun divMixed(a: UInt, b: UShort): UInt = a / b

fun box(): String {
    if (div(7u, 2u) != 3u) return "fail: 7u / 2u"
    if (rem(7u, 2u) != 1u) return "fail: 7u % 2u"
    // Above Int.MAX_VALUE: a signed division of the same bits would answer a negative quotient.
    if (div(4000000000u, 3u) != 1333333333u) return "fail: 4000000000u / 3u"
    if (rem(4000000000u, 7u) != 3u) return "fail: 4000000000u % 7u"
    if (div(UInt.MAX_VALUE, 0x10000u) != 0xFFFFu) return "fail: UInt.MAX_VALUE / 0x10000u"
    if (div(200.toUByte(), 3.toUByte()) != 66u) return "fail: 200 / 3 as UByte"
    if (rem(60000.toUShort(), 7.toUShort()) != 3u) return "fail: 60000 % 7 as UShort"
    if (divMixed(3000000000u, 60000.toUShort()) != 50000u) return "fail: UInt / UShort"
    return "OK"
}
