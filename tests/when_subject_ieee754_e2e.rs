//! A `when` subject smart-cast by an earlier `is`/`!is` arm compares later values as that
//! primitive. `-0.0 == 0.0` is true for IEEE equality and false for boxed `equals`.

use super::common::expect_box_same_as_kotlinc;

/// `ieee754/smartCastOnWhenSubjectAfterCheckInBranch_properIeeeComparisons.kt`.
#[test]
fn smart_cast_when_subject_uses_ieee754_equality() {
    expect_box_same_as_kotlinc(
        r#"
fun testF(x: Any) =
    when (x) {
        !is Float -> "!Float"
        0.0F -> "0.0"
        else -> "other"
    }

fun testD(x: Any) =
    when (x) {
        !is Double -> "!Double"
        0.0 -> "0.0"
        else -> "other"
    }

fun box(): String {
    val tf = testF(-0.0F)
    if (tf != "0.0") return "Fail 1: $tf"
    val td = testD(-0.0)
    if (td != "0.0") return "Fail 2: $td"
    return "OK"
}
"#,
        "WhenSubjectIeee",
    );
}

/// `when/whenSubjectVariable/ieee754EqualityWithSmartCast.kt`: a `Float` subject compared with the
/// `Double` literal `0.0` widens after the smart cast, and a `Double` upper bound already compares
/// as a primitive.
#[test]
fn smart_cast_when_subject_widens_to_the_double_literal() {
    expect_box_same_as_kotlinc(
        r#"
val az: Any = -0.0
val afz: Any = -0.0f

fun box(): String {
    val y = az
    when (y) {
        !is Double -> throw AssertionError()
        0.0 -> {}
        else -> throw AssertionError()
    }
    val yy = afz
    when (yy) {
        !is Float -> throw AssertionError()
        0.0 -> {}
        else -> throw AssertionError()
    }
    testDoubleAsUpperBound(-0.0)
    return "OK"
}

fun <T: Double> testDoubleAsUpperBound(v: T): Boolean {
    return when (val a = v*v) {
        0.0 -> true
        else -> throw AssertionError()
    }
}
"#,
        "WhenSubjectIeeeWiden",
    );
}
