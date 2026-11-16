// Members of builtin scalars whose library bodies take the scalar itself as the receiver: an
// explicit `equals` and `Boolean.toString`. `(-0.0).equals(0.0)` is false and
// `Double.NaN.equals(Double.NaN)` is true, unlike `==`, and a `Float` never equals a `Double`.

fun doubles(a: Double, b: Double) = a.equals(b)

fun floats(a: Float, b: Any?) = a.equals(b)

fun box(): String {
    if (doubles(-0.0, 0.0)) return "fail -0.0"
    if (!doubles(Double.NaN, Double.NaN)) return "fail NaN"
    if (!floats(0.9999f, 0.9999f) || floats(1.0f, 1.0)) return "fail Float"
    if (true.toString() != "true" || false.toString() != "false") return "fail Boolean"
    return "OK"
}
