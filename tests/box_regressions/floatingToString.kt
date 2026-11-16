// Double and Float print the same text on every target for values written in plain notation.

data class Point(val x: Double, val y: Float)

fun show(value: Any?): String = value.toString()

fun describe(value: Double?): String = "value=$value"

fun check(actual: String, expected: String): String? =
    if (actual == expected) null else "expected <$expected>, got <$actual>"

fun box(): String {
    val third = 1.0 / 3.0
    val tenth = 0.1f
    check(1.5.toString(), "1.5")?.let { return it }
    check("${0.1}", "0.1")?.let { return it }
    check("${-2.25}", "-2.25")?.let { return it }
    check("${0.0}", "0.0")?.let { return it }
    check("${-0.0}", "-0.0")?.let { return it }
    check("${100.0}", "100.0")?.let { return it }
    check("${123456.789}", "123456.789")?.let { return it }
    check("${0.001}", "0.001")?.let { return it }
    check("$third", "0.3333333333333333")?.let { return it }
    check("${0.1 + 0.2}", "0.30000000000000004")?.let { return it }
    check("${Double.NaN}", "NaN")?.let { return it }
    check("${Double.POSITIVE_INFINITY}", "Infinity")?.let { return it }
    check("${Double.NEGATIVE_INFINITY}", "-Infinity")?.let { return it }
    check("$tenth", "0.1")?.let { return it }
    check(1.1f.toString(), "1.1")?.let { return it }
    check("${-3.75f}", "-3.75")?.let { return it }
    check("${Float.NaN}", "NaN")?.let { return it }
    check(show(2.5), "2.5")?.let { return it }
    check(show(0.5f), "0.5")?.let { return it }
    check(describe(4.125), "value=4.125")?.let { return it }
    check(describe(null), "value=null")?.let { return it }
    check("${Point(1.5, 2.5f)}", "Point(x=1.5, y=2.5)")?.let { return it }
    return "OK"
}
