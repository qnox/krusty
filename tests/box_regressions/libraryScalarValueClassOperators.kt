// The stdlib unsigned operators construct their value class from the underlying value and read
// its storage property back; a backend that carries the class as that value evaluates both as the
// value itself. Their decimal strings are a separate conversion and stay out of this case.

fun uintSum(x: UInt): UInt = x + 1u
fun ulongProduct(x: ULong, y: ULong): ULong = x * y
fun ubyteSum(x: UByte, y: UByte): UInt = x + y
fun ushortDifference(x: UShort, y: UShort): UInt = x - y

fun box(): String {
    if (uintSum(4294967294u) != 4294967295u) return "uint plus"
    if (uintSum(4294967295u) != 0u) return "uint plus wraps"
    if (ulongProduct(3uL, 5uL) != 15uL) return "ulong times"
    if (ubyteSum(200u, 100u) != 300u) return "ubyte plus"
    if (ushortDifference(5u, 7u) != 4294967294u) return "ushort minus"
    if (4294967295u.compareTo(1u) <= 0) return "uint compareTo"
    if (!(1u < 4294967295u)) return "uint less"
    if (!(18446744073709551615uL > 1uL)) return "ulong greater"
    if (!(UByte.MAX_VALUE > 1.toUByte())) return "ubyte greater"
    if (!(UShort.MAX_VALUE > 1.toUShort())) return "ushort greater"
    return "OK"
}
