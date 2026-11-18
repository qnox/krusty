// KOTLINC_VERSION: [2.5.0,)
// LANGUAGE: +ContextParameters +CallableReferencesToContextual
// A type variable that only a context parameter mentions is inferred from the value the reference
// binds for it, as it is for a call.

context(c: C)
fun <C> contextToString(): String = c.toString()

context(c: C)
fun <C> pairWith(value: Int): String = "$c:$value"

fun box(): String {
    context(9) {
        val numeric: () -> String = ::contextToString
        if (numeric() != "9") return "FAIL 1: ${numeric()}"
        val paired: (Int) -> String = ::pairWith
        if (paired(2) != "9:2") return "FAIL 2: ${paired(2)}"
    }
    context("s") {
        val text: () -> String = ::contextToString
        if (text() != "s") return "FAIL 3: ${text()}"
    }
    return "OK"
}
