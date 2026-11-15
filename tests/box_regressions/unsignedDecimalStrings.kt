fun byteDirect(value: UByte): String = value.toString()
fun shortDirect(value: UShort): String = value.toString()
fun intDirect(value: UInt): String = value.toString()
fun longDirect(value: ULong): String = value.toString()
fun byteTemplate(value: UByte): String = "$value"
fun shortTemplate(value: UShort): String = "$value"
fun intTemplate(value: UInt): String = "$value"
fun longTemplate(value: ULong): String = "$value"

fun checkByte(value: UByte, expected: String): Boolean = byteDirect(value) == expected && byteTemplate(value) == expected
fun checkShort(value: UShort, expected: String): Boolean = shortDirect(value) == expected && shortTemplate(value) == expected
fun checkInt(value: UInt, expected: String): Boolean = intDirect(value) == expected && intTemplate(value) == expected
fun checkLong(value: ULong, expected: String): Boolean = longDirect(value) == expected && longTemplate(value) == expected
fun signedInt(value: Int): String = value.toString() + ":" + "$value"
fun signedLong(value: Long): String = value.toString() + ":" + "$value"

var effects = 0
fun nextByte(): UByte { effects = effects + 1; return 255u }
fun nextShort(): UShort { effects = effects + 1; return 65535u }
fun nextInt(): UInt { effects = effects + 1; return 4294967295u }
fun nextLong(): ULong { effects = effects + 1; return 18446744073709551615uL }

fun box(): String {
    if (!checkByte(0u, "0")) return "byte zero"
    if (!checkByte(128u, "128")) return "byte high bit"
    if (!checkByte(255u, "255")) return "byte maximum"
    if (!checkShort(0u, "0")) return "short zero"
    if (!checkShort(32768u, "32768")) return "short high bit"
    if (!checkShort(65535u, "65535")) return "short maximum"
    if (!checkInt(0u, "0")) return "int zero"
    if (!checkInt(2147483648u, "2147483648")) return "int high bit"
    if (!checkInt(4294967295u, "4294967295")) return "int maximum"
    if (!checkLong(0uL, "0")) return "long zero"
    if (!checkLong(9223372036854775808uL, "9223372036854775808")) return "long high bit"
    if (!checkLong(18446744073709551615uL, "18446744073709551615")) return "long maximum"
    if (signedInt(-1) != "-1:-1") return "signed int negative"
    if (signedInt(-2147483647 - 1) != "-2147483648:-2147483648") return "signed int minimum"
    if (signedLong(-1L) != "-1:-1") return "signed long negative"
    if (signedLong(-9223372036854775807L - 1L) != "-9223372036854775808:-9223372036854775808") return "signed long minimum"
    effects = 0
    if (nextByte().toString() != "255" || effects != 1) return "byte direct effect"
    if ("${nextByte()}" != "255" || effects != 2) return "byte template effect"
    if (nextShort().toString() != "65535" || effects != 3) return "short direct effect"
    if ("${nextShort()}" != "65535" || effects != 4) return "short template effect"
    if (nextInt().toString() != "4294967295" || effects != 5) return "int direct effect"
    if ("${nextInt()}" != "4294967295" || effects != 6) return "int template effect"
    if (nextLong().toString() != "18446744073709551615" || effects != 7) return "long direct effect"
    if ("${nextLong()}" != "18446744073709551615" || effects != 8) return "long template effect"
    return "OK"
}
