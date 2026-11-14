@file:OptIn(ExperimentalUnsignedTypes::class)

import kotlin.jvm.JvmInline

OPTIONAL_JVM_INLINE_ANNOTATION
value class Bits(val raw: IntArray)
OPTIONAL_JVM_INLINE_ANNOTATION
value class Longs(val raw: LongArray)
OPTIONAL_JVM_INLINE_ANNOTATION
value class NBits(val raw: IntArray?)
OPTIONAL_JVM_INLINE_ANNOTATION
value class Names(val raw: Array<String>)
OPTIONAL_JVM_INLINE_ANNOTATION
value class NNames(val raw: Array<Int>?)
OPTIONAL_JVM_INLINE_ANNOTATION
value class UBits(val raw: UIntArray)
OPTIONAL_JVM_INLINE_ANNOTATION
value class Uns(val raw: Array<UInt>)

fun box(): String {
    val shared = intArrayOf(1, 2)
    val texts = listOf(
        Bits(shared).toString(),
        Longs(longArrayOf(3L)).toString(),
        NBits(intArrayOf(4)).toString(),
        NBits(null).toString(),
        Names(arrayOf("a", "b")).toString(),
        NNames(arrayOf(5)).toString(),
        NNames(null).toString(),
        UBits(uintArrayOf(1u, 4000000000u)).toString(),
        Uns(arrayOf(6u)).toString(),
        "in ${Bits(shared)}",
        (Bits(shared).hashCode() == shared.contentHashCode()).toString(),
        (Bits(shared) == Bits(shared)).toString(),
        (Bits(shared) == Bits(intArrayOf(1, 2))).toString(),
    ).joinToString(";")
    val expected = "Bits(raw=[1, 2]);Longs(raw=[3]);NBits(raw=[4]);NBits(raw=null);" +
        "Names(raw=[a, b]);NNames(raw=[5]);NNames(raw=null);UBits(raw=UIntArray(storage=[1, -294967296]));" +
        "Uns(raw=[6]);in Bits(raw=[1, 2]);true;true;false"
    return if (texts == expected) "OK" else texts
}
