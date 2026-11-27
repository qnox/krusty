// WITH_STDLIB
// FILE: Declaration.kt
interface Consumer {
    fun consume(s: String)
}

var log = ""

inline fun plain(text: String): Consumer = object : Consumer {
    override fun consume(s: String) {
        log = log + text + s
    }
}

fun call(block: () -> String) = block()

inline fun twice(text: String): String = call { text + text }

fun declaringFile(): String {
    plain("").consume("x")
    return twice("y")
}
// FILE: Caller.kt
fun box(): String {
    if (declaringFile() != "yy") return "declaring file"
    log = ""
    plain("").consume("O")
    if (twice("K") != "KK") return "lambda"
    return log + "K"
}
