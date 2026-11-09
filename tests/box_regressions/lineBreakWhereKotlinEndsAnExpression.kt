// kotlinc's parser ends an expression at a line break unless the next token continues it (`.`,
// `?.`, `?:`, `as`, `&&`, `||`): an operator, call parentheses, a `return` value, or a call suffix
// after a safe-call selector on the next line belong to a new statement. A label is defined only by
// a name that touches its `@`, so a getter ending in a name keeps the next line's annotation for
// the next declaration.
class Annotated {
    val first: String
        get() = value
    @Suppress("UNUSED")
    val second: String = "K"
    private val value = "O"
}

class Log {
    var entries = 0
}

fun earlyReturn(log: Log) {
    if (log.entries == 0) return
    log.entries = log.entries + 1
}

class Text(val content: String) {
    val size: Int get() = 2
}

@Suppress("UNUSED_VARIABLE", "UNUSED_EXPRESSION")
fun box(): String {
    val difference = 2
    - 1
    val function = { value: Int -> value }
    val notCalled = function
    (3)
    val text: Text? = Text("OK")
    val safeSize = text?.size
    (text)
    val log = Log()
    earlyReturn(log)
    val continued = text
        ?.size
        ?: 0
    val annotated = Annotated()
    val ok = difference == 2
        && notCalled === function
        && safeSize == 2
        && log.entries == 0
        && continued == 2
        && annotated.first + annotated.second == "OK"
    return if (ok) "OK" else "FAIL"
}
