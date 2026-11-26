// KOTLINC_VERSION: [2.5.0,)
// LANGUAGE: +ContextParameters +CallableReferencesToContextual

fun box(): String {
    var captured = "O"

    context(s: String)
    fun local(suffix: String) = captured + s + suffix

    context(s: String, n: Int)
    fun two(a: String, b: Int = 7) = captured + s + n + a + b

    val r = context("_") {
        val ref: (String) -> String = ::local
        ref("K")
    }
    if (r != "O_K") return "FAIL 1: $r"
    captured = "Q"
    val natural = context("-") { ::local }
    if (natural("Z") != "Q-Z") return "FAIL 2: ${natural("Z")}"
    val d = context("a", 1) { val f: (String) -> String = ::two; f }
    if (d("b") != "Qa1b7") return "FAIL 3: ${d("b")}"
    return "OK"
}
