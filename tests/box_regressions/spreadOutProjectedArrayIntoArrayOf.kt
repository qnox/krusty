class Name(val text: String)
class Code(val text: String)

fun parts(named: Boolean): Array<out Any> =
    if (named) arrayOf(Name("n")) else arrayOf(Code("c"))

fun third(vararg items: Any): Any = items[2]

fun box(): String {
    val leading = arrayOf<Any>(1, "s")
    val projected = parts(true)
    val strings: Array<out String> = arrayOf("x", "y")

    val joined = arrayOf(*leading, *projected, "p")
    if ((third(*joined) as Name).text != "n") return "joined"
    if (joined[3] != "p") return "joined tail"

    val tail = arrayOf(*projected, 3)
    if (tail[1] != 3) return "tail"

    val typed: Array<Any> = arrayOf(*strings, 4)
    if (typed[0] != "x" || typed[2] != 4) return "typed"

    val explicit = arrayOf<CharSequence>(*strings)
    if (explicit[1] != "y") return "explicit"

    val named = arrayOf(elements = strings)
    if (named[1] != "y") return "named"

    return "OK"
}
