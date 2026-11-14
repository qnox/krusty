// WITH_STDLIB
fun box(): String {
    var byte: UByte = 255u
    byte++
    if (byte != 0u.toUByte()) return "ubyte increment"
    var short: UShort = 65535u
    short++
    if (short != 0u.toUShort()) return "ushort increment"
    return "OK"
}
