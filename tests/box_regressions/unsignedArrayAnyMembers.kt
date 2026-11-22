// An unsigned array is a value class over the signed array of its width. Its generated `toString`
// and `hashCode` answer through that carrier on all four widths, called directly, through string
// concatenation and a template, and through an `Any` box.
@file:OptIn(ExperimentalUnsignedTypes::class)

fun bytes(a: UByteArray): String = a.toString()
fun shorts(a: UShortArray): String = a.toString()
fun ints(a: UIntArray): String = a.toString()
fun longs(a: ULongArray): String = a.toString()
fun joined(a: UIntArray): String = "storage " + a.toString()
fun template(b: UByteArray, s: UShortArray, i: UIntArray, l: ULongArray): String = "$b|$s|$i|$l"
fun boxed(a: UIntArray): String { val any: Any = a; return any.toString() }
fun hashes(b: UByteArray, s: UShortArray, i: UIntArray, l: ULongArray): Int =
    b.hashCode() + s.hashCode() + i.hashCode() + l.hashCode()

fun box(): String {
    val b = ubyteArrayOf(1u, 200u)
    val s = ushortArrayOf(1u, 65535u)
    val i = uintArrayOf(1u, 4000000000u)
    val l = ulongArrayOf(1uL, 18446744073709551615uL)
    val texts = listOf(
        bytes(b),
        shorts(s),
        ints(i),
        longs(l),
        joined(i),
        template(b, s, i, l),
        boxed(i),
        (hashes(b, s, i, l) == hashes(b, s, i, l)).toString(),
        (i.hashCode() == (i as Any).hashCode()).toString(),
    ).joinToString(",")
    val expected = "UByteArray(storage=[1, -56]),UShortArray(storage=[1, -1])," +
        "UIntArray(storage=[1, -294967296]),ULongArray(storage=[1, -1])," +
        "storage UIntArray(storage=[1, -294967296])," +
        "UByteArray(storage=[1, -56])|UShortArray(storage=[1, -1])|" +
        "UIntArray(storage=[1, -294967296])|ULongArray(storage=[1, -1])," +
        "UIntArray(storage=[1, -294967296]),true,true"
    return if (texts == expected) "OK" else texts
}
