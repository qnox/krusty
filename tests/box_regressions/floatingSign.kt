import kotlin.math.sign

var signInputs = 0

fun floatInput(value: Float): Float {
    signInputs++
    return value
}

fun doubleInput(value: Double): Double {
    signInputs++
    return value
}

fun checkFloat(value: Float, expected: Float): Boolean {
    val before = signInputs
    val fromFunction = sign(floatInput(value))
    val fromProperty = floatInput(value).sign
    if (signInputs != before + 2) return false
    if (expected.isNaN()) return fromFunction.isNaN() && fromProperty.isNaN()
    return fromFunction.toRawBits() == expected.toRawBits() &&
        fromProperty.toRawBits() == expected.toRawBits()
}

fun checkDouble(value: Double, expected: Double): Boolean {
    val before = signInputs
    val fromFunction = sign(doubleInput(value))
    val fromProperty = doubleInput(value).sign
    if (signInputs != before + 2) return false
    if (expected.isNaN()) return fromFunction.isNaN() && fromProperty.isNaN()
    return fromFunction.toRawBits() == expected.toRawBits() &&
        fromProperty.toRawBits() == expected.toRawBits()
}

fun box(): String {
    if (!checkFloat(-2.5f, -1.0f) || !checkFloat(2.5f, 1.0f)) return "float finite"
    if (!checkFloat(-0.0f, -0.0f) || !checkFloat(0.0f, 0.0f)) return "float zero"
    if (!checkFloat(Float.NEGATIVE_INFINITY, -1.0f) ||
        !checkFloat(Float.POSITIVE_INFINITY, 1.0f)) return "float infinity"
    if (!checkFloat(Float.NaN, Float.NaN)) return "float NaN"
    if (!checkDouble(-2.5, -1.0) || !checkDouble(2.5, 1.0)) return "double finite"
    if (!checkDouble(-0.0, -0.0) || !checkDouble(0.0, 0.0)) return "double zero"
    if (!checkDouble(Double.NEGATIVE_INFINITY, -1.0) ||
        !checkDouble(Double.POSITIVE_INFINITY, 1.0)) return "double infinity"
    if (!checkDouble(Double.NaN, Double.NaN)) return "double NaN"
    return "OK"
}
