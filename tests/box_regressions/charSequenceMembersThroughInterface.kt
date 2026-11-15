// A call of a library interface's member on a value read as that interface runs the
// implementation of the value's own class: a string's, or a class of the program's.
class Letters(private val text: String) : CharSequence {
    override val length: Int get() = text.length + 1
    override fun get(index: Int): Char = if (index == text.length) '!' else text[index]
    override fun subSequence(startIndex: Int, endIndex: Int): CharSequence = this
}

fun size(value: CharSequence): Int = value.length

fun first(value: CharSequence): Char = value[0]

fun last(value: CharSequence): Char = value[value.length - 1]

fun <T : CharSequence> boundedSize(value: T): Int = value.length

fun box(): String {
    if (size("four") != 4) return "string length"
    if (first("OK") != 'O') return "string get"
    if (last("OK") != 'K') return "string last"
    if (boundedSize("abc") != 3) return "bounded string"
    val letters = Letters("ab")
    if (size(letters) != 3) return "class length"
    if (last(letters) != '!') return "class get"
    if (boundedSize(letters) != 3) return "bounded class"
    return "OK"
}
