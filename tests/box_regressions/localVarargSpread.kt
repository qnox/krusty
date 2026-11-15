// A local function's arguments map onto its parameters as every selected call's do. A spread
// passes its whole array to the vararg, alone, between elements, after a fixed parameter, before a
// parameter that follows the vararg, and to a local extension. Plain elements before a named
// parameter that follows the vararg all belong to the vararg, also for a local extension, and an
// empty call leaves a vararg that is not the last parameter empty, also for a local function with
// a context parameter. A spread that does not fit the local vararg leaves the local function
// inapplicable, so the enclosing function is selected.
class Tag(val name: String)

class Scale(val factor: Int)

fun strings(xs: Array<String>): String {
    fun at(vararg parts: String, index: Int): String = parts[index]
    fun first(vararg parts: String): String = parts[0]
    fun prefixed(head: Int, vararg parts: String): String = head.toString() + parts[1]
    return first(*xs) + at(*xs, index = 1) + at("a", *xs, "b", index = 3) + at(*xs, *xs, index = 2) +
        prefixed(1, *xs) + at("p", "q", index = 0)
}

fun leading(): String {
    fun choose(vararg values: String, ok: Boolean = true): String = values.size.toString() + ok
    return choose() + choose("a", "b") + choose("c", ok = false)
}

fun Tag.extension(xs: Array<String>): String {
    fun Tag.render(vararg parts: String): String = name + parts[0]
    fun Tag.at(vararg parts: String, index: Int): String = name + parts[index]
    return render(*xs) + render("z", *xs) + at("a", "b", index = 1) + at(*xs, index = 0)
}

fun scaled(xs: Array<String>): Int {
    context(scale: Scale)
    fun count(vararg n: Any, extra: Int = 0): Int = n.size * scale.factor + extra
    return with(Scale(2)) { count("a", "b") + count("c", extra = 1) + count() + count(*xs, "d") }
}

fun pick(vararg values: Any): String = "outer" + values.size

fun mismatched(tags: Array<Tag>): String {
    fun pick(vararg values: String): String = "local" + values.size
    return pick(*tags) + pick("s")
}

fun box(): String {
    if (strings(arrayOf("x", "y")) != "xybx1yp") return "Fail strings"
    if (leading() != "0true2true1false") return "Fail leading " + leading()
    if (Tag("t").extension(arrayOf("p", "q")) != "tptztbtp") return "Fail extension"
    if (scaled(arrayOf("e", "f")) != 4 + 3 + 0 + 6) return "Fail scaled"
    if (mismatched(arrayOf(Tag("u"), Tag("v"))) != "outer2local1") return "Fail mismatched"
    return "OK"
}
