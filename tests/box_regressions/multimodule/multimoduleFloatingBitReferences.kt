// MODULE: bits
@Suppress("INVISIBLE_REFERENCE", "INVISIBLE_MEMBER")
@kotlin.internal.InlineOnly
inline fun <T, R> T.throughReference(block: (T) -> R): R = block(this)
fun floatFromBitsReference(value: Int): Float = value.throughReference(Float::fromBits)
fun doubleFromBitsReference(value: Long): Double = value.throughReference(Double::fromBits)
// MODULE: main(bits)
fun box(): String {
    if (floatFromBitsReference(1065353216) != 1.0f) return "float reference"
    if (doubleFromBitsReference(4607182418800017408L) != 1.0) return "double reference"
    return "OK"
}
