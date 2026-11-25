// KOTLINC_VERSION: [2.5.0,)
// LANGUAGE: +ContextParameters +CallableReferencesToContextual

context(s: String)
fun go(): (String) -> String {
    var captured = "O"
    context(t: String)
    fun local(suffix: String) = captured + t + s + suffix
    val ref: (String) -> String = ::local
    captured = "Q"
    return ref
}

fun box(): String {
    val ref = context("_") { go() }
    val r = ref("K")
    return if (r != "Q__K") "FAIL value: $r" else "OK"
}
