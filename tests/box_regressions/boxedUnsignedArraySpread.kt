// A spread into an unsigned vararg reads the array's carrier (`int[]` for `UIntArray`). A
// `UIntArray` that arrives as its box (a smart cast from `Any`, a nullable `UIntArray?` local)
// is unboxed before the copy or the spread builder sees it, alone or beside other elements.
@file:OptIn(ExperimentalUnsignedTypes::class)

fun uints(vararg xs: UInt): UInt = xs.sum()
fun ulongs(vararg xs: ULong): ULong = xs.sum()

fun spreadSmartCast(a: Any): UInt = if (a is UIntArray) uints(*a) else 0u
fun spreadMixed(a: Any): UInt = if (a is UIntArray) uints(1u, *a) else 0u
fun spreadTwice(a: Any): UInt = if (a is UIntArray) uints(*a, *a) else 0u
fun spreadLongSmartCast(a: Any): ULong = if (a is ULongArray) ulongs(*a) else 0uL
fun spreadNullable(a: UIntArray?): UInt = if (a != null) uints(*a) else 0u
fun spreadNullableMixed(a: UIntArray?): UInt = if (a != null) uints(*a, 1u) else 0u

fun box(): String {
    val xs = uintArrayOf(1u, 2u)
    if (spreadSmartCast(xs) != 3u) return "fail smart cast"
    if (spreadMixed(xs) != 4u) return "fail mixed"
    if (spreadTwice(xs) != 6u) return "fail twice"
    if (spreadLongSmartCast(ulongArrayOf(3uL, 4uL)) != 7uL) return "fail long"
    if (spreadNullable(xs) != 3u) return "fail nullable"
    if (spreadNullableMixed(xs) != 4u) return "fail nullable mixed"
    if (spreadNullable(null) != 0u) return "fail null"
    return "OK"
}
