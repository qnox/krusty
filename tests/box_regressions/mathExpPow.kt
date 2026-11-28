import kotlin.math.abs
import kotlin.math.exp
import kotlin.math.pow

private fun near(expected: Double, actual: Double): Boolean = abs(expected - actual) <= 1e-12 * abs(expected)

private fun nearFloat(expected: Float, actual: Float): Boolean = abs(expected - actual) <= 1e-5f * abs(expected)

fun box(): String {
    if (2.0.pow(10.0) != 1024.0) return "fail pow integers"
    if (2.0f.pow(10.0f) != 1024.0f) return "fail powf integers"
    if (!near(1.4142135623730951, 2.0.pow(0.5))) return "fail pow root"
    if (!nearFloat(1.4142135f, 2.0f.pow(0.5f))) return "fail powf root"
    if (0.0.pow(-1.0) != Double.POSITIVE_INFINITY) return "fail pow zero"
    if ((-8.0).pow(1.0 / 3.0).isNaN().not()) return "fail pow negative base"
    if (Double.NaN.pow(0.0) != 1.0) return "fail pow nan exponent zero"

    if (exp(0.0) != 1.0) return "fail exp zero"
    if (!near(2.718281828459045, exp(1.0))) return "fail exp one"
    if (!nearFloat(2.7182817f, exp(1.0f))) return "fail expf one"
    if (exp(-1000.0) != 0.0) return "fail exp underflow"
    if (exp(1000.0) != Double.POSITIVE_INFINITY) return "fail exp overflow"

    return "OK"
}
