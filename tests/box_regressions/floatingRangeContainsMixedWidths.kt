var rangeCalls = 0
var valueCalls = 0

fun doubleRange(): ClosedFloatingPointRange<Double> {
    rangeCalls++
    return 1.0..3.0
}

fun floatValue(): Float {
    valueCalls++
    return 2.0f
}

fun box(): String {
    if (!doubleRange().contains(floatValue())) return "float in double range"
    if (rangeCalls != 1 || valueCalls != 1) return "repeated operand"
    val floats: ClosedFloatingPointRange<Float> = 1.0f..3.0f
    if (!floats.contains(2.0)) return "double in float range"
    if (floats.contains(4.0)) return "outside"
    if (floats.contains(0.0 / 0.0)) return "nan"
    val empty: ClosedFloatingPointRange<Double> = 3.0..1.0
    if (empty.contains(2.0f)) return "empty"
    return "OK"
}
