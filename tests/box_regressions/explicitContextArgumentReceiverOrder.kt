// LANGUAGE: +ContextParameters +ExplicitContextArguments
// A context parameter is passed before an extension receiver. Named arguments that reorder a
// contextual extension call still evaluate in source order; an argument with nothing to order (a
// constant, a parameter) keeps its place, and an implicit context argument orders nothing.
var log = ""

fun logged(label: String): Int {
    log += label
    return label.length
}

fun receiver(): Double {
    log += "r"
    return 1.0
}

context(c: Int)
fun Double.contextual(value: Int): Int = c + value + toInt()

fun take(): String {
    val result = log
    log = ""
    return result
}

fun fromParameter(d: Double): Int = d.contextual(c = logged("c"), value = logged("v"))

fun box(): String {
    receiver().contextual(value = logged("v"), c = logged("c"))
    take().let { if (it != "rvc") return "reordered context: $it" }
    receiver().contextual(c = 5, value = logged("v"))
    take().let { if (it != "rv") return "constant context: $it" }
    fromParameter(2.0)
    take().let { if (it != "cv") return "parameter receiver: $it" }
    val sum = context(7) { receiver().contextual(logged("v")) }
    take().let { if (it != "rv") return "implicit context: $it" }
    if (sum != 9) return "sum $sum"
    return "OK"
}
