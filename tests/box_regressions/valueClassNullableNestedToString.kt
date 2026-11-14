@file:OptIn(ExperimentalUnsignedTypes::class)
import kotlin.jvm.JvmInline
OPTIONAL_JVM_INLINE_ANNOTATION value class S(val v: String)
OPTIONAL_JVM_INLINE_ANNOTATION value class V(val s: S?)
OPTIONAL_JVM_INLINE_ANNOTATION value class NU(val raw: UIntArray?)
OPTIONAL_JVM_INLINE_ANNOTATION value class NL(val raw: ULongArray?)
OPTIONAL_JVM_INLINE_ANNOTATION value class I(val x: Int)
OPTIONAL_JVM_INLINE_ANNOTATION value class NI(val raw: I?)
OPTIONAL_JVM_INLINE_ANNOTATION value class NUI(val raw: UInt?)
fun box(): String {
    val text = listOf(
        V(S("a")).toString(), V(null).toString(),
        NU(uintArrayOf(1u, 4000000000u)).toString(), NU(null).toString(),
        NL(ulongArrayOf(2uL)).toString(), NL(null).toString(),
        NI(I(3)).toString(), NI(null).toString(),
        NUI(4u).toString(), NUI(null).toString(),
        "in ${V(S("b"))}",
    ).joinToString(";")
    val expected = "V(s=S(v=a));V(s=null);" +
        "NU(raw=UIntArray(storage=[1, -294967296]));NU(raw=null);" +
        "NL(raw=ULongArray(storage=[2]));NL(raw=null);" +
        "NI(raw=I(x=3));NI(raw=null);NUI(raw=4);NUI(raw=null);in V(s=S(v=b))"
    return if (text == expected) "OK" else text
}
