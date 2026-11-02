@file:OptIn(ExperimentalUnsignedTypes::class)

package unsignedannotations

const val UB_MAX: UByte = 255u
const val US_MAX: UShort = 65535u
const val UI_MAX: UInt = 4294967295u
const val UL_MAX: ULong = 18446744073709551615uL

@Target(AnnotationTarget.CLASS, AnnotationTarget.TYPE)
annotation class Bits(
    val b: UByteArray,
    val s: UShortArray,
    val i: UIntArray,
    val l: ULongArray,
    val signed: ByteArray,
)

@Bits(
    ubyteArrayOf(0u, 128u, UB_MAX),
    ushortArrayOf(0u, 32768u, US_MAX),
    uintArrayOf(0u, 2147483648u, UI_MAX),
    ulongArrayOf(0uL, 9223372036854775808uL, UL_MAX),
    byteArrayOf(-1),
)
class Subject

fun box(): String = "OK"
