// WITH_STDLIB

class Letters(private val letters: String) : CharSequence {
    override val length: Int get() = letters.length
    override fun get(index: Int): Char = letters[index]
    override fun subSequence(startIndex: Int, endIndex: Int): CharSequence {
        val copy = StringBuilder()
        for (index in startIndex until endIndex) copy.append(letters[index])
        return Letters(copy.toString())
    }
    override fun toString(): String = letters
}

fun box(): String {
    val builder = StringBuilder()
    val text: CharSequence = "xOy"
    builder.append(text, 1, 2)
    val nothing: CharSequence? = null
    builder.append(nothing, 0, 0)
    val other = StringBuilder("zzK")
    builder.append(other, 2, 3)
    builder.append(Letters("--!"), 1, 1)
    val absent: CharSequence? = null
    builder.append(absent, 1, 3)
    builder.append(Letters("ab"), 0, 2)
    val expected = "OKul" + "ab"
    return if (builder.toString() == expected) "OK" else "FAIL: $builder"
}
