class Name(val text: String)
class Code(val text: String)

fun parts(named: Boolean): Array<out Any> =
    if (named) arrayOf(Name("n")) else arrayOf(Code("c"))

fun third(vararg items: Any): Any = items[2]

fun box(): String {
    val one = Code("1")
    val leading = arrayOf<Any>(one, "s")
    val projected = parts(true)
    val strings: Array<out String> = arrayOf("x", "y")

    val joined = arrayOf(*leading, *projected, "p")
    if ((third(*joined) as Name).text != "n") return "joined"
    if (joined[0] !== one || joined[3] != "p") return "joined ends"

    val last = Code("3")
    val tail = arrayOf(*projected, last)
    if (tail[1] !== last) return "tail"

    val four = Code("4")
    val typed: Array<Any> = arrayOf(*strings, four)
    if (typed[0] != "x" || typed[2] !== four) return "typed"

    val explicit = arrayOf<CharSequence>(*strings)
    if (explicit[1] != "y") return "explicit"

    val named = arrayOf(elements = strings)
    if (named[1] != "y") return "named"

    return "OK"
}
