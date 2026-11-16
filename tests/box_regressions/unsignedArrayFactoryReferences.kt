@file:OptIn(ExperimentalUnsignedTypes::class)
fun box(): String {
    val fn = ::uintArrayOf
    val whole = fn(uintArrayOf(42u))
    if (whole[0] != 42u) return "fail whole"

    val adapted: (ULong, ULong) -> ULongArray = ::ulongArrayOf
    val a = adapted(1uL, 2uL)
    if (!a.contentEquals(ulongArrayOf(1uL, 2uL))) return "fail adapted " + a.contentToString()

    val bytes: (UByte, UByte) -> UByteArray = ::ubyteArrayOf
    if (!bytes(3u, 4u).contentEquals(ubyteArrayOf(3u, 4u))) return "fail ubyte"
    val shorts: (UShort) -> UShortArray = ::ushortArrayOf
    if (shorts(5u).size != 1) return "fail ushort"
    return "OK"
}
