import kotlin.jvm.JvmInline

OPTIONAL_JVM_INLINE_ANNOTATION
value class Uns(val raw: Array<UInt>)
OPTIONAL_JVM_INLINE_ANNOTATION
value class Longs(val raw: Array<ULong>)
OPTIONAL_JVM_INLINE_ANNOTATION
value class NUns(val raw: Array<UInt>?)
OPTIONAL_JVM_INLINE_ANNOTATION
value class Shorts(val raw: Array<UShort?>)
class Holder(val raw: Array<UInt>, val bytes: Array<UByte>?)
fun take(a: Array<UInt>, b: Array<UByte>?): Int = a.size + (b?.size ?: 0)

fun box(): String {
    val texts = listOf(
        Uns(arrayOf(1u, 4000000000u)).toString(),
        Longs(arrayOf(18446744073709551615uL)).toString(),
        NUns(null).toString(),
        Shorts(arrayOf(65535u, null)).toString(),
        Holder(arrayOf(2u), arrayOf(3.toUByte())).raw[0].toString(),
        take(arrayOf(1u, 2u), null).toString(),
    ).joinToString(";")
    val expected = "Uns(raw=[1, 4000000000]);Longs(raw=[18446744073709551615]);NUns(raw=null);" +
        "Shorts(raw=[65535, null]);2;2"
    return if (texts == expected) "OK" else texts
}
