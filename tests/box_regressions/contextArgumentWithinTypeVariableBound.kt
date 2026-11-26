// LANGUAGE: +ContextParameters
// A context parameter whose type is a bounded type variable takes the nearest context value
// within the bound. A nearer value outside the bound is not a candidate for it.

class Tag(val name: String)

context(c: C, s: String)
fun <C : Tag> tagged(value: Int): String = c.name + s + value

context(c: C)
fun <C : CharSequence> length(): Int = c.length

fun box(): String {
    val result = context(Tag("t"), "-") { tagged(1) }
    if (result != "t-1") return "FAIL 1: $result"
    val size = context("abc", Tag("x")) { length() }
    if (size != 3) return "FAIL 2: $size"
    return "OK"
}
