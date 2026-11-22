// A callable reference passed to `this(...)` is typed against the selected target's parameter,
// so an overloaded `println` is not ambiguous when the target has defaulted parameters.

var out = ""

fun record(text: String) {
    out += text
}

fun record(value: Int) {
    out += value
}

fun record() {
    out += "!"
}

open class Logger(
    val identity: String?,
    val output: (String) -> Unit,
    val isColored: Boolean = true,
) {
    constructor(identity: String?) : this(identity, ::record)

    constructor(identity: String?, flag: Boolean) : this(identity, ::record, flag)
}

fun box(): String {
    Logger("a").output("x")
    Logger("b", false).output("y")
    if (out != "xy") return "out: $out"
    if (Logger("c", false).isColored) return "flag"
    return "OK"
}
