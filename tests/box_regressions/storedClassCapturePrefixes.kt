private var trace = ""

private fun forwarded(): String {
    trace += "F"
    return "P"
}

open class Probe(marker: String) {
    val observed = read()
    init { trace += marker }
    open fun read(): String = "base"
}

fun box(): String {
    trace = ""
    val text = "K"
    val number = 7
    val probe = object : Probe(forwarded()) {
        val own = "O"
        override fun read(): String = if (text == "K" && number == 7) "OK" else "capture"
        fun result(): String = own + text
    }
    if (probe.observed != "OK" || probe.result() != "OK" || trace != "FP") {
        return "order:" + trace + ":" + probe.observed + ":" + probe.result()
    }
    val outer = object {
        fun read(): String = (object { fun read(): String = text }).read()
    }
    return if (outer.read() == "K") "OK" else "nested"
}
