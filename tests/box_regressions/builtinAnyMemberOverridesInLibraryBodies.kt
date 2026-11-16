// A library body calling a builtin's own override of an `Any` member (`Double.equals`,
// `Float.equals`, `Int.equals`) reaches the value's own equality, as kotlinc's does: `NaN` equals
// itself and the two zeros differ, unlike `==` on the primitive.

fun box(): String {
    if (!doubleArrayOf(Double.NaN).contentEquals(doubleArrayOf(Double.NaN))) return "double NaN"
    if (doubleArrayOf(0.0).contentEquals(doubleArrayOf(-0.0))) return "double zeros"
    if (!doubleArrayOf(1.5, 2.0).contentEquals(doubleArrayOf(1.5, 2.0))) return "double"
    if (!floatArrayOf(Float.NaN).contentEquals(floatArrayOf(Float.NaN))) return "float NaN"
    if (floatArrayOf(0f).contentEquals(floatArrayOf(-0f))) return "float zeros"
    if (!intArrayOf(1, -2).contentEquals(intArrayOf(1, -2))) return "int"
    if (longArrayOf(3L).contentEquals(longArrayOf(4L))) return "long"
    return "OK"
}
