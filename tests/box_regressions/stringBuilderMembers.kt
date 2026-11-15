// A `StringBuilder` built, appended to, read and rendered from source: every constructor, the
// `String?`, `Char` and `CharSequence?` appends (`null` included), growth past the capacity asked
// for, `length`, the index operator, `toString()` and string templates over a nullable builder.

fun describe(sb: StringBuilder?): String = "[$sb]"

fun box(): String {
    val sb = StringBuilder()
    sb.append("O").append('K')
    val empty: String? = null
    sb.append(empty)
    val seq: CharSequence = "abcdef"
    sb.append(seq).append("bc")
    val nested = StringBuilder("xy")
    sb.append(nested as CharSequence)
    val absent: CharSequence? = null
    sb.append(absent)
    val sized = StringBuilder(2)
    for (i in 0 until 40) sized.append('z')
    val fromSeq = StringBuilder(seq)
    val text = sb.toString()
    if (text != "OKnullabcdefbcxynull") return "FAIL text: $text"
    if (sb.length != 20 || sb[1] != 'K') return "FAIL length/get: ${sb.length}"
    if (sized.length != 40 || sized[39] != 'z') return "FAIL grow: ${sized.length}"
    if ("$fromSeq|${describe(null)}|${describe(nested)}" != "abcdef|[null]|[xy]") return "FAIL template"
    return "" + sb[0] + sb[1]
}
