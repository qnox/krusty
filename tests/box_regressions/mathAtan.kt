import kotlin.math.PI
import kotlin.math.atan
import kotlin.math.atan2

fun box(): String {
    if (atan(1.0) != PI / 4) return "fail atan"
    if (atan(0.0) != 0.0 || atan(Double.POSITIVE_INFINITY) != PI / 2) return "fail atan limits"
    if (atan2(1.0, 1.0) != PI / 4) return "fail atan2 diagonal"
    if (atan2(1.0, -1.0) != 3 * PI / 4) return "fail atan2 second quadrant"
    if (atan2(-1.0, -1.0) != -3 * PI / 4) return "fail atan2 third quadrant"
    if (atan2(0.0, -1.0) != PI || atan2(-0.0, -1.0) != -PI) return "fail atan2 pi"
    if (atan2(1.0, 0.0) != PI / 2) return "fail atan2 half pi"
    if (atan(-1.0) != -PI / 4 || atan(Double.NEGATIVE_INFINITY) != -PI / 2) return "fail atan negative"
    if (!atan(Double.NaN).isNaN() || !atan2(Double.NaN, 1.0).isNaN() || !atan2(1.0, Double.NaN).isNaN()) return "fail nan"
    if (1.0 / atan(-0.0) != Double.NEGATIVE_INFINITY) return "fail atan negative zero"
    if (atan2(0.0, 0.0) != 0.0 || 1.0 / atan2(-0.0, 0.0) != Double.NEGATIVE_INFINITY) return "fail atan2 zero zero"
    if (atan2(0.0, -0.0) != PI || atan2(-0.0, -0.0) != -PI) return "fail atan2 zero negative zero"
    val inf = Double.POSITIVE_INFINITY
    if (atan2(inf, inf) != PI / 4 || atan2(inf, -inf) != 3 * PI / 4) return "fail atan2 infinities"
    if (atan2(1.0, inf) != 0.0 || atan2(1.0, -inf) != PI || atan2(-inf, 1.0) != -PI / 2) return "fail atan2 infinite operand"
    return "OK"
}
