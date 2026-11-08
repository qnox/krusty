@file:OptIn(ExperimentalUnsignedTypes::class)

fun box(): String {
    val b = ubyteArrayOf(1u, 200u)
    if (b.toString() != "UByteArray(storage=[1, -56])") return "fail ubyte: $b"
    val s = ushortArrayOf(1u, 65535u)
    if (s.toString() != "UShortArray(storage=[1, -1])") return "fail ushort: $s"
    val i = uintArrayOf(1u, 4000000000u)
    if (i.toString() != "UIntArray(storage=[1, -294967296])") return "fail uint: $i"
    val l = ulongArrayOf(1uL, 18446744073709551615uL)
    if (l.toString() != "ULongArray(storage=[1, -1])") return "fail ulong: $l"
    if ("$i" != "UIntArray(storage=[1, -294967296])") return "fail template: $i"
    if (UIntArray(0).toString() != "UIntArray(storage=[])") return "fail empty"
    if (i != i || !i.equals(i)) return "fail self equality"
    if (i == uintArrayOf(1u, 4000000000u)) return "fail content equality"
    if (b == ubyteArrayOf(1u, 200u) || l == ulongArrayOf(1uL, 18446744073709551615uL)) return "fail content equality"
    if (i.hashCode() != i.hashCode()) return "fail hash stability"
    return "OK"
}
