// A spread passes its whole array to a local function's vararg, alone, between elements, after a
// fixed parameter, before a parameter that follows the vararg, and to a local extension. Plain
// elements before a named parameter that follows the vararg all belong to the vararg.
class Tag(val name: String)

fun strings(xs: Array<String>): String {
    fun at(vararg parts: String, index: Int): String = parts[index]
    fun first(vararg parts: String): String = parts[0]
    fun prefixed(head: Int, vararg parts: String): String = head.toString() + parts[1]
    return first(*xs) + at(*xs, index = 1) + at("a", *xs, "b", index = 3) + at(*xs, *xs, index = 2) +
        prefixed(1, *xs) + at("p", "q", index = 0)
}

fun Tag.extension(xs: Array<String>): String {
    fun Tag.render(vararg parts: String): String = name + parts[0]
    return render(*xs) + render("z", *xs)
}

fun box(): String {
    if (strings(arrayOf("x", "y")) != "xybx1yp") return "Fail strings"
    if (Tag("t").extension(arrayOf("p", "q")) != "tptz") return "Fail extension"
    return "OK"
}
