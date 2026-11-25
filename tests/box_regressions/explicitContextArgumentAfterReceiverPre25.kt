// LANGUAGE: +ContextParameters +ExplicitContextArguments
// KOTLINC_VERSION: [2.4.0,2.5.0)
// A context parameter is passed before an extension receiver. kotlinc before 2.5 evaluates the
// operands in the order the call passes them: the context argument, then the receiver, then the
// value.
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

fun box(): String {
    val sum = receiver().contextual(c = logged("c"), value = logged("v"))
    if (log != "crv") return "evaluation order: $log"
    if (sum != 3) return "sum $sum"
    return "OK"
}
