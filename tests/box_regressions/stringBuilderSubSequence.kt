// `CharSequence.subSequence` on a `StringBuilder` copies the range of the units in use and rejects
// one outside them.
fun box(): String {
    val builder = StringBuilder("hello world")
    builder.append("!!")
    val text: CharSequence = builder
    if (text.subSequence(6, 11).toString() != "world") return "fail middle"
    if (text.subSequence(0, 0).length != 0) return "fail empty"
    if (text.subSequence(0, text.length).toString() != "hello world!!") return "fail whole"
    builder.append('?')
    if (text.subSequence(11, 14).toString() != "!!?") return "fail after append"
    try {
        text.subSequence(5, 99)
        return "fail past the end"
    } catch (e: IndexOutOfBoundsException) {
    }
    try {
        text.subSequence(4, 3)
        return "fail reversed"
    } catch (e: IndexOutOfBoundsException) {
    }
    return "OK"
}
