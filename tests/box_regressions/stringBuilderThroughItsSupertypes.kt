// A `StringBuilder` is an `Appendable` and a `CharSequence`: a call of one of their members on a
// builder held as that supertype runs the builder's own override, as `joinToString` does when it
// appends each element to the builder it creates.
fun appendTo(out: Appendable): Appendable =
    out.append('O').append("K").append(null as CharSequence?).append(StringBuilder("!"))

fun describe(text: CharSequence): String = "" + text.length + text[0] + text[text.length - 1]

fun box(): String {
    val builder = StringBuilder()
    val returned = appendTo(builder)
    if (returned !== builder) return "FAIL append answers its builder"
    if (builder.toString() != "OKnull!") return "FAIL appended: $builder"
    if (describe(builder) != "7O!") return "FAIL builder as CharSequence: ${describe(builder)}"
    if (describe("text") != "4tt") return "FAIL string as CharSequence"
    val joined = arrayOf("O", "K").joinToString("")
    if (joined != "OK") return "FAIL joined: $joined"
    val listed = arrayOf("1", "2", "3").joinToString(separator = ", ", prefix = "[", postfix = "]")
    if (listed != "[1, 2, 3]") return "FAIL listed: $listed"
    return "OK"
}
