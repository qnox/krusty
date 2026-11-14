// `compareTo` on floating values is a total order: `-0.0` is below `0.0`, `NaN` equals itself and is
// above every other value, and the ordinary comparison operators keep IEEE semantics.
fun box(): String {
    val nan = Double.NaN
    if (1.0.compareTo(2.0) != -1) return "double less"
    if (2.0.compareTo(1.0) != 1) return "double greater"
    if (1.5.compareTo(1.5) != 0) return "double equal"
    if ((-0.0).compareTo(0.0) != -1) return "negative zero below zero"
    if (0.0.compareTo(-0.0) != 1) return "zero above negative zero"
    if (nan.compareTo(1.0) != 1) return "nan above finite"
    if (nan.compareTo(Double.POSITIVE_INFINITY) != 1) return "nan above infinity"
    if (1.0.compareTo(nan) != -1) return "finite below nan"
    if (nan.compareTo(nan) != 0) return "nan equals nan"
    if (Double.NEGATIVE_INFINITY.compareTo(-1.0) != -1) return "negative infinity"
    if ((-2.0).compareTo(-1.0) != -1) return "negative order"
    if (nan == nan) return "nan operator"
    val fnan = Float.NaN
    if (1.0f.compareTo(2.0f) != -1) return "float less"
    if (2.0f.compareTo(1.0f) != 1) return "float greater"
    if ((-0.0f).compareTo(0.0f) != -1) return "float negative zero"
    if (fnan.compareTo(Float.POSITIVE_INFINITY) != 1) return "float nan above infinity"
    if (fnan.compareTo(fnan) != 0) return "float nan equals nan"
    if ((-2.0f).compareTo(-1.0f) != -1) return "float negative order"
    return "OK"
}
