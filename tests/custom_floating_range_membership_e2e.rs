//! `x in a..b` for `Double`/`Float` is a direct IEEE comparison only when `rangeTo` is the
//! stdlib floating range. A nearer operator is `rangeTo` then `contains`.
use super::common::expect_box_same_as_kotlinc;

#[test]
fn a_same_file_double_range_to_uses_compare_to_contains() {
    const SRC: &str = "\
operator fun Double.rangeTo(other: Double): ClosedRange<Double> =
    object : ClosedRange<Double> {
        override val start: Double = this@rangeTo
        override val endInclusive: Double = other
    }

fun member(value: Double, start: Double, end: Double): String {
    val direct = value in start..end
    val range = start..end
    val stored = value in range
    return when {
        direct != stored -> \"disagree\"
        direct -> \"in\"
        else -> \"out\"
    }
}

fun box(): String {
    if (member(-0.0, 0.0, 0.0) != \"out\") return \"zero\"
    if (member(Double.NaN, Double.NaN, Double.NaN) != \"in\") return \"nan\"
    return \"OK\"
}
";
    expect_box_same_as_kotlinc(SRC, "CustomDoubleRange");
}

#[test]
fn a_same_file_float_range_to_uses_its_contains() {
    const SRC: &str = "\
operator fun Float.rangeTo(other: Float): ClosedFloatingPointRange<Float> =
    object : ClosedFloatingPointRange<Float> {
        override val start: Float = other
        override val endInclusive: Float = this@rangeTo
        override fun lessThanOrEquals(a: Float, b: Float): Boolean = a >= b
    }

fun member(value: Float, start: Float, end: Float): String {
    val direct = value in start..end
    val range = start..end
    val stored = value in range
    return when {
        direct != stored -> \"disagree\"
        direct -> \"in\"
        else -> \"out\"
    }
}

fun box(): String {
    if (member(-0.0f, 0.0f, 0.0f) != \"in\") return \"zero\"
    if (member(Float.NaN, Float.NaN, Float.NaN) != \"out\") return \"nan\"
    return \"OK\"
}
";
    expect_box_same_as_kotlinc(SRC, "CustomFloatRange");
}

#[test]
fn stdlib_double_membership_keeps_ieee_comparison() {
    const SRC: &str = "\
fun box(): String {
    val zero = -0.0 in 0.0..0.0
    val nan = Double.NaN in Double.NaN..Double.NaN
    return if (zero && !nan) \"OK\" else \"fail\"
}
";
    expect_box_same_as_kotlinc(SRC, "StdlibDoubleRange");
}
