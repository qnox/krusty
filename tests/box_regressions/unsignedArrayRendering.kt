@file:OptIn(ExperimentalUnsignedTypes::class)

// An unsigned array renders as the library class does: `UIntArray(storage=[1, 2, -1])`, also when it
// is a value class's or a data class's property, and it keeps its own identity when stored as `Any`
// (`is UIntArray`, not `is IntArray`). An `Array<UInt>` renders its elements.
OPTIONAL_JVM_INLINE_ANNOTATION
value class Elements(val a: Array<UInt>)

OPTIONAL_JVM_INLINE_ANNOTATION
value class Wrapped(val a: UIntArray)

data class Holder(val a: UIntArray)

fun box(): String {
    val u = uintArrayOf(1u, 2u, UInt.MAX_VALUE)
    val any: Any = u
    val rendered = (if (any is UIntArray) "is" else "not") + ";" +
        (any is IntArray) + ";" +
        any.toString() + ";" +
        Elements(arrayOf(1u)).toString() + ";" +
        Wrapped(u).toString() + ";" +
        Holder(u).toString() + ";" +
        "$u"
    val expected = "is;false;UIntArray(storage=[1, 2, -1]);Elements(a=[1]);Wrapped(a=UIntArray(storage=[1, 2, -1]));Holder(a=UIntArray(storage=[1, 2, -1]));UIntArray(storage=[1, 2, -1])"
    val bytes = ubyteArrayOf(255u, 1u)
    val shorts = ushortArrayOf(65535u)
    val longs = ulongArrayOf(ULong.MAX_VALUE)
    val narrow = "$bytes$shorts$longs"
    if (narrow != "UByteArray(storage=[-1, 1])UShortArray(storage=[-1])ULongArray(storage=[-1])") return narrow
    return if (rendered == expected) "OK" else rendered
}
