// LANGUAGE: +ContextParameters +ExplicitContextArguments
// KOTLINC_VERSION: [2.5.0,)
// A context parameter is passed before an extension receiver. Since 2.5 kotlinc evaluates a
// receiver written before an explicit context argument first, then the operands in source order.
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
    if (log != "rcv") return "evaluation order: $log"
    if (sum != 3) return "sum $sum"
    return "OK"
}
