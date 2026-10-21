// MODULE: bits
fun floatBits(value: Float) = value.toRawBits()
fun floatCanonicalBits(value: Float) = value.toBits()
fun floatFromBits(value: Int) = Float.fromBits(value)
fun doubleBits(value: Double) = value.toRawBits()
fun doubleCanonicalBits(value: Double) = value.toBits()
fun doubleFromBits(value: Long) = Double.fromBits(value)
// MODULE: main(bits)
fun box(): String {
    if (floatBits(1.0f) != 1065353216) return "float"
    if (doubleFromBits(4607182418800017408L) != 1.0) return "double"
    val floatNan = floatFromBits(2139095041)
    if (floatBits(floatNan) != 2139095041) return "float raw nan"
    if (floatCanonicalBits(floatNan) != 2143289344) return "float canonical nan"
    val doubleNan = doubleFromBits(9218868437227405313L)
    if (doubleBits(doubleNan) != 9218868437227405313L) return "double raw nan"
    if (doubleCanonicalBits(doubleNan) != 9221120237041090560L) return "double canonical nan"
    return "OK"
}
