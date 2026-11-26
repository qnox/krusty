// A property without a declared type whose initializer calls a function of the same name: the
// property is being inferred, so it cannot supply an `invoke` callee, and the call reaches the
// function a lower scope level declares.
fun stamp(format: String?): String = "s" + format

class Logger {
    private val stamp get() = stamp(null)
    fun render(): String = stamp
}

enum class Color {
    RED, GREEN;

    companion object {
        private val values = values()
        fun count(): Int = values.size
    }
}

fun box(): String {
    if (Logger().render() != "snull") return "property: ${Logger().render()}"
    if (Color.count() != 2) return "enum: ${Color.count()}"
    return "OK"
}
