private var trace = ""
private fun edge(mark: String, value: String): String { trace += mark; return value }

fun box(): String {
    trace = ""
    val prefix = edge("C", "O")
    val value = ({ left: String, right: String ->
        trace += "B"
        prefix + left + right
    })(edge("A", "K"), edge("D", ""))
    if (trace != "CADB" || value != "OK") return "order:" + trace + ":" + value
    val localReturn = (fun(suffix: String): String {
        if (suffix == "K") return prefix + suffix
        return "local return"
    })("K")
    return localReturn
}
