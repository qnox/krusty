// WITH_STDLIB
// FILE: Declaration.kt
interface Consumer {
    fun consume(s: String)
}

inline fun cross(crossinline block: (String) -> Unit): Consumer = object : Consumer {
    override fun consume(s: String) {
        block(s)
    }
}

var log = ""

inline fun note(text: String) {
    log = log + text
}
// FILE: Caller.kt
fun logged(): Consumer = cross { log = log + it }

fun box(): String {
    logged().consume("O")
    note("K")
    return log
}
