// WITH_STDLIB
fun box(): String {
    val boolean: Boolean? = true
    if (boolean!! != true) return "boolean"
    val byte: Byte? = 7
    if (byte!! != 7.toByte()) return "byte"
    val short: Short? = 7
    if (short!! != 7.toShort()) return "short"
    val char: Char? = 'a'
    if (char!! != 'a') return "char"
    val int: Int? = 7
    if (int!! != 7) return "int"
    val long: Long? = 7L
    if (long!! != 7L) return "long"
    val float: Float? = 1.5f
    if (float!! != 1.5f) return "float"
    val double: Double? = 1.5
    if (double!! != 1.5) return "double"
    val ubyte: UByte? = 7u
    if (ubyte!! != 7u.toUByte()) return "ubyte"
    val ushort: UShort? = 7u
    if (ushort!! != 7u.toUShort()) return "ushort"
    val uint: UInt? = 7u
    if (uint!! != 7u) return "uint"
    val ulong: ULong? = 7uL
    if (ulong!! != 7uL) return "ulong"
    return "OK"
}
