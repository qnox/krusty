@file:OptIn(ExperimentalUnsignedTypes::class)
fun box(): String {
    val ints = UIntArray(3)
    ints[0] = 4000000000u
    ints[1] = 7u
    if (ints.size != 3) return "fail ints size"
    if (ints[0] != 4000000000u || ints[1] != 7u || ints[2] != 0u) return "fail ints"

    val longs = ulongArrayOf(18446744073709551615uL, 2uL)
    if (longs[0] != 18446744073709551615uL || longs[1] != 2uL || longs.size != 2) return "fail longs"

    val bytes = ubyteArrayOf(255u, 3u)
    if (bytes[0] != 255u.toUByte() || bytes[1] != 3u.toUByte()) return "fail bytes"

    val shorts = ushortArrayOf(65535u, 1u)
    shorts[1] = 65534u
    if (shorts[0] != 65535u.toUShort() || shorts[1] != 65534u.toUShort()) return "fail shorts"

    var sum = 0u
    for (x in uintArrayOf(7u, 8u)) sum += x
    if (sum != 15u) return "fail loop"

    return "OK"
}
