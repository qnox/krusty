// Unsigned numbers read as nullable values and as `Any`: each is a box of its own classifier.

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
    return "OK"
}
