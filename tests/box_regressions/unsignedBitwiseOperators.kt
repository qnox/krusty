// Unsigned `and`, `or` and `xor` are sign-agnostic: each answers the bitwise result of its
// operands' two's-complement carriers, including values above the signed range.

fun box(): String {
    val a = 0xF0F0_0000u
    val b = 0x0FF0_00FFu
    if (a and b != 0x00F0_0000u) return "UInt.and"
    if (a or b != 0xFFF0_00FFu) return "UInt.or"
    if (a xor b != 0xFF00_00FFu) return "UInt.xor"
    val c = 0xF000_0000_0000_00F0uL
    val d = 0x8000_0000_0000_0F0FuL
    if (c and d != 0x8000_0000_0000_0000uL) return "ULong.and"
    if (c or d != 0xF000_0000_0000_0FFFuL) return "ULong.or"
    if (c xor d != 0x7000_0000_0000_0FFFuL) return "ULong.xor"
    return "OK"
}
