// WITH_STDLIB
fun narrow(u: UByte): Byte = u.toByte()
fun narrowShort(u: UShort): Short = u.toShort()
fun box(): String {
    if (narrow(255u).toInt() != -1) return "UByte.toByte kept zero-extended bits"
    if (narrow(255u) != (-1).toByte()) return "UByte.toByte compared unequal"
    if (narrowShort(65535u).toInt() != -1) return "UShort.toShort kept zero-extended bits"
    return "OK"
}
