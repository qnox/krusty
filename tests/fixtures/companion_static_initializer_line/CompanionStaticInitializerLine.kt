// A companion property's initializer that starts on a later line than its `val`, or that spans
// several lines, records the initializer's lines in the class initializer.

class Holder {
    companion object {
        private val folded =
            "x" +
                "y"
        private val called =
            "p".padStart(
                3,
                '-',
            )
        val chained =
            "abc"
                .uppercase()
                .length
        val sameLine = 5
    }

    fun describe(): String = folded + called + chained + sameLine
}

fun box(): String {
    val described = Holder().describe()
    if (described != "xy--p35") return "fail: $described"
    return "OK"
}
