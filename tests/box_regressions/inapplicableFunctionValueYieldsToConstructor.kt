class label(val x: String)

fun asConstructor(label: (Int) -> String): String = label("x").x

fun asValue(label: (String) -> String): String = label("x")

fun box(): String {
    val constructed = asConstructor { "no" }
    if (constructed != "x") return "constructor:$constructed"
    val value = asValue { "value:$it" }
    if (value != "value:x") return "value:$value"
    return "OK"
}
