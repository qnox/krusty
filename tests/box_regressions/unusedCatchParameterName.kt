// The catch parameter `_` declares no name; its local variable table entry is `<unused var>`.

fun parse(text: String): Int =
    try {
        text.toInt()
    } catch (_: NumberFormatException) {
        -1
    }

fun box(): String {
    var failures = 0
    try {
        error("boom")
    } catch (_: IllegalStateException) {
        failures++
    }
    if (failures != 1) return "failures $failures"
    return if (parse("12") == 12 && parse("x") == -1) "OK" else "parse"
}
