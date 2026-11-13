// Unsigned numbers read as nullable values, as `Any`, and as reference array elements: each is a
// box of its own classifier.

fun nullableUInt(value: UInt?): UInt? = value

fun keep(value: Any?): Any? = value

fun box(): String {
    val present = nullableUInt(5u)
    if (present == null) return "fail: 5u became null"
    if (present != 5u) return "fail: 5u round-trip"
    if (nullableUInt(null) != null) return "fail: null round-trip"
    val fallback: UInt = nullableUInt(null) ?: 7u
    if (fallback != 7u) return "fail: elvis"

    val longValue: ULong? = 9uL
    if ((longValue ?: 0uL) != 9uL) return "fail: ULong?"
    val byteValue: UByte? = 200u.toUByte()
    if (byteValue != 200u.toUByte()) return "fail: UByte?"
    val shortValue: UShort? = 60000u.toUShort()
    if (shortValue != 60000u.toUShort()) return "fail: UShort?"

    val any: Any = 5u
    val same: Any = 5u
    val other: Any = 6u
    val signed: Any = 5
    if (any != same) return "fail: UInt as Any equality"
    if (any == other) return "fail: UInt as Any inequality"
    if (any == signed) return "fail: UInt equals Int"
    if (any.hashCode() != 5u.hashCode()) return "fail: UInt hashCode"
    if (keep(5uL) != keep(5uL)) return "fail: ULong as Any"
    if (keep(5uL) == keep(5L)) return "fail: ULong equals Long"
    if (keep(0x1_0000_0001uL).hashCode() != 0x1_0000_0001uL.hashCode()) return "fail: ULong hashCode"
    if (keep(200u.toUByte()).hashCode() != 200u.toUByte().hashCode()) return "fail: UByte hashCode"
    if (keep(60000u.toUShort()).hashCode() != 60000u.toUShort().hashCode()) return "fail: UShort hashCode"
    val filled = Array(2) { 7u }
    filled[1] = 9u
    if (filled[0] != 7u) return "fail: filled[0]"
    if (filled[1] != 9u) return "fail: filled[1]"
    if (filled[0] + filled[1] != 16u) return "fail: filled sum"
    val empty: Array<UInt?> = Array(2) { null }
    empty[0] = 5u
    if (empty[0] != 5u) return "fail: empty[0]"
    if (empty[1] != null) return "fail: empty[1]"
    val literal = arrayOf<UInt?>(1u, null)
    var sum = 0u
    for (x in literal) sum += x ?: 0u
    if (sum != 1u) return "fail: literal sum"
    return "OK"
}
