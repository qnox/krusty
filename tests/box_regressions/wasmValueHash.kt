fun textHash(value: String): Int = value.hashCode()
fun byteHash(value: Byte): Int = value.hashCode()
fun shortHash(value: Short): Int = value.hashCode()
fun intHash(value: Int): Int = value.hashCode()
fun longHash(value: Long): Int = value.hashCode()
fun charHash(value: Char): Int = value.hashCode()
fun booleanHash(value: Boolean): Int = value.hashCode()
fun floatHash(value: Float): Int = value.hashCode()
fun doubleHash(value: Double): Int = value.hashCode()

fun box(): String {
    if (textHash("") != 0 || textHash("Aa") != 2112 || textHash("BB") != 2112) return "string"
    if (textHash("λ😃") != 2690657 || textHash("😃") != 1772902) return "UTF16"
    if (byteHash(-17) != -17 || shortHash(-300) != -300 || intHash(-70000) != -70000) return "signed"
    if (longHash(4294967297L) != 0 || longHash(4294967298L) != 3) return "long halves"
    if (charHash('Ω') != 937) return "char"
    if (booleanHash(true) != 1231 || booleanHash(false) != 1237) return "boolean"
    if (floatHash(0.0f) != 0 || floatHash(-0.0f) != (-2147483647 - 1)) return "float zero"
    if (floatHash(0.0f / 0.0f) != 2143289344 || floatHash(1.5f) != 1069547520) return "float bits"
    if (doubleHash(0.0) != 0 || doubleHash(-0.0) != (-2147483647 - 1)) return "double zero"
    if (doubleHash(0.0 / 0.0) != 2146959360) return "double NaN"
    return "OK"
}
