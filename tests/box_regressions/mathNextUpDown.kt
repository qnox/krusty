import kotlin.math.nextDown
import kotlin.math.nextUp

// The adjacent representable value, which the Native runtime answers rather than a stdlib body.
// `Float.nextUp` and `Float.nextDown` are not declared by the Wasm stdlib, so the `Float` forms are
// covered by `mathNextUpDownFloat.kt`.
fun box(): String {
    if (1.0.nextUp() != 1.0000000000000002) return "Fail double up"
    if (1.0.nextDown() != 0.9999999999999999) return "Fail double down"
    if (0.0.nextUp() != Double.MIN_VALUE) return "Fail zero up"
    if ((-0.0).nextDown() != -Double.MIN_VALUE) return "Fail negative zero down"
    if (Double.MAX_VALUE.nextUp() != Double.POSITIVE_INFINITY) return "Fail max up"
    if (Double.POSITIVE_INFINITY.nextUp() != Double.POSITIVE_INFINITY) return "Fail infinity up"
    if (Double.NEGATIVE_INFINITY.nextDown() != Double.NEGATIVE_INFINITY) return "Fail negative infinity down"
    if (Double.POSITIVE_INFINITY.nextDown() != Double.MAX_VALUE) return "Fail infinity down"
    if (!Double.NaN.nextUp().isNaN() || !Double.NaN.nextDown().isNaN()) return "Fail double NaN"

    return "OK"
}
