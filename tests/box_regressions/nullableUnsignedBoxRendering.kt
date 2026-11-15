@file:OptIn(ExperimentalUnsignedTypes::class)

fun renderUnsignedBox(value: Any?): String = "$value"

fun box(): String {
    val byte: UByte? = UByte.MAX_VALUE
    val short: UShort? = UShort.MAX_VALUE
    val int: UInt? = UInt.MAX_VALUE
    val long: ULong? = ULong.MAX_VALUE
    if ("$byte;$short;$int;$long" != "255;65535;4294967295;18446744073709551615") return "nullable values"
    if (renderUnsignedBox(byte) != "255") return "erased UByte"
    if (renderUnsignedBox(short) != "65535") return "erased UShort"
    if (renderUnsignedBox(int) != "4294967295") return "erased UInt"
    if (renderUnsignedBox(long) != "18446744073709551615") return "erased ULong"
    val absent: UInt? = null
    if ("$absent" != "null" || renderUnsignedBox(absent) != "null") return "null value"
    return "OK"
}
