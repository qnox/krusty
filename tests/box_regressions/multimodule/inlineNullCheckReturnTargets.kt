// MODULE: lib
fun requireText(value: String?): String = requireNotNull(value)
fun checkNumber(value: Int?): Int = checkNotNull(value)
// MODULE: main(lib)
fun box(): String {
    var calls = 0
    fun once(): String? { calls++; return "O" }
    val text = requireText(once())
    if (calls != 1 || text != "O") return "evaluation order"
    if (checkNumber(7) != 7) return "scalar result"
    try {
        requireText(null)
        return "missing require failure"
    } catch (e: Throwable) {
        if (e.message != "Required value was null.") return "require message"
    }
    try {
        checkNumber(null)
        return "missing check failure"
    } catch (e: Throwable) {
        if (e.message != "Required value was null.") return "check message"
    }
    return text + "K"
}
