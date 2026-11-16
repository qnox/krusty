// Type tests and safe casts against primitive and unsigned types on values read as `Any?`: each
// box is an instance of its own classifier only, never of another type carrying the same bits.

fun kind(value: Any?): String = when (value) {
    is Char -> "C"
    is Int -> "I"
    is Long -> "L"
    is Short -> "S"
    is UInt -> "U"
    is Boolean -> "B"
    is Double -> "D"
    is String -> "T"
    null -> "N"
    else -> "?"
}

fun <T> isChar(value: T): Boolean = value is Char

fun box(): String {
    val values = arrayOf<Any?>('x', 1, 2L, 3.toShort(), 4u, true, 5.0, "s", null, Unit)
    var kinds = ""
    for (value in values) kinds += kind(value)
    if (kinds != "CILSUBDTN?") return "Fail kinds: $kinds"
    if (!isChar('c') || isChar(99)) return "Fail generic"
    val number: Any = 65
    if (number is Char) return "Fail Int is Char"
    if (number !is Int) return "Fail Int !is Int"
    if ((number as? Long) != null) return "Fail Int as? Long"
    if ((number as? Int) != 65) return "Fail Int as? Int"
    val nullable: Any? = null
    if (nullable !is Int?) return "Fail null !is Int?"
    return "OK"
}
