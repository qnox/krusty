// MODULE: library
// FILE: Library.kt
package fixture

open class Formatter(private val prefix: String) {
    open fun format(value: Int, unit: String = "u", scale: Int = value * 2): String =
        "$prefix$value$unit$scale"
}

class Loud(prefix: String) : Formatter(prefix) {
    override fun format(value: Int, unit: String, scale: Int): String =
        "!" + super.format(value, unit + unit, scale)
}

// The calls dispatch on the formatter's class and leave arguments to the defaults its open member
// declares, which the override inherits.
fun formatAll(formatter: Formatter): String =
    formatter.format(1) + "," + formatter.format(2, "m") + "," + formatter.format(3, scale = 0)

fun formatBoth(): String = formatAll(Formatter("a")) + ";" + formatAll(Loud("b"))

// MODULE: main(library)
// FILE: Consumer.kt
package fixture

fun box(): String {
    val result = formatBoth()
    return if (result == "a1u2,a2m4,a3u0;!b1uu2,!b2mm4,!b3uu0") "OK" else result
}
