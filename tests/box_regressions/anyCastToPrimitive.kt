// `as` from a reference to a primitive reads the value out of its box, whether the operand is
// nullable (`Any?`) or not (`Any`).
fun asInt(value: Any?): Int = value as Int
fun asLong(value: Any): Long = value as Long
fun asChar(value: Any?): Char = value as Char
fun asBoolean(value: Any): Boolean = value as Boolean
fun asDouble(value: Any?): Double = value as Double

class Holder(private val held: Any?) {
    val size: Int get() = held as Int
}

fun box(): String {
    if (asInt(40) + 2 != 42) return "fail Int"
    if (asLong(1L shl 40) != 1099511627776L) return "fail Long"
    if (asChar('K') != 'K') return "fail Char"
    if (!asBoolean(true)) return "fail Boolean"
    if (asDouble(1.5) * 2 != 3.0) return "fail Double"
    if (Holder(7).size != 7) return "fail property"
    return "OK"
}
