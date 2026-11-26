// TARGET_BACKEND: JVM_IR, NATIVE
import kotlin.math.nextDown
import kotlin.math.nextUp

// The `Float` forms of the adjacent representable value, which the Native runtime answers rather
// than a stdlib body.
//
// PLATFORM-SPECIFIC: the Wasm stdlib declares no `Float.nextUp` or `Float.nextDown`, so the shared
// `mathNextUpDown.kt` regression carries the `Double` forms and this one is limited to the
// backends whose stdlib declares both.
fun box(): String {
    if (1.0f.nextUp() != 1.0000001f) return "Fail up"
    if (1.0f.nextDown() != 0.99999994f) return "Fail down"
    if (0.0f.nextUp() != Float.MIN_VALUE) return "Fail zero up"
    if ((-0.0f).nextDown() != -Float.MIN_VALUE) return "Fail negative zero down"
    if (Float.MAX_VALUE.nextUp() != Float.POSITIVE_INFINITY) return "Fail max up"
    if (Float.POSITIVE_INFINITY.nextUp() != Float.POSITIVE_INFINITY) return "Fail infinity up"
    if (Float.NEGATIVE_INFINITY.nextDown() != Float.NEGATIVE_INFINITY) return "Fail negative infinity down"
    if (Float.POSITIVE_INFINITY.nextDown() != Float.MAX_VALUE) return "Fail infinity down"
    if (!Float.NaN.nextUp().isNaN() || !Float.NaN.nextDown().isNaN()) return "Fail NaN"
    return "OK"
}
