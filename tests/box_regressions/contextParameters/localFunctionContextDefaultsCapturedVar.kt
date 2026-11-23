// KOTLINC_VERSION: [2.5.0,)
// LANGUAGE: +ContextParameters

fun box(): String {
    var captured = "Q"

    context(s: String)
    fun two(a: String, b: Int = 7) = captured + s + a + b

    val d = context("a") { two("b") }
    if (d != "Qab7") return "FAIL 3: $d"
    return "OK"
}
