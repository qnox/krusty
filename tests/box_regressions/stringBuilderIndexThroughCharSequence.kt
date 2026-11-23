// `CharSequence.get` on a `StringBuilder` held as a `CharSequence` runs the builder's override,
// which throws `IndexOutOfBoundsException` for an index outside the units in use, whether past the
// end, negative, or within the spare capacity a builder keeps.
fun charAt(text: CharSequence, index: Int): Char = text[index]

fun rejects(text: CharSequence, index: Int): Boolean =
    try {
        charAt(text, index)
        false
    } catch (e: IndexOutOfBoundsException) {
        true
    }

fun box(): String {
    val builder = StringBuilder(16).append('O').append('K')
    if (charAt(builder, 1) != 'K') return "FAIL in range: ${charAt(builder, 1)}"
    if (!rejects(builder, 2)) return "FAIL past the end"
    if (!rejects(builder, 15)) return "FAIL within the spare capacity"
    if (!rejects(builder, -1)) return "FAIL negative"
    return "" + charAt(builder, 0) + charAt(builder, 1)
}
