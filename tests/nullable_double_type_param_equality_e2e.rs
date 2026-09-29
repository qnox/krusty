//! `==` on type parameters bounded by `Double` uses IEEE equality. A bound of `Any` stays
//! boxed `equals`, so `-0.0` and `0.0` differ there.

use super::common::expect_box_same_as_kotlinc;

/// `binaryOp/eqNullableDoublesWithTP.kt`.
#[test]
fn nullable_double_type_parameters_use_ieee754_equality() {
    expect_box_same_as_kotlinc(
        r#"
fun <A: Double, B: Double?> eq_double_doubleN(a: A, b: B) = a == b

fun <A: Double, B: Any> eq_double_any(a: A, b: B) = a == b

fun <A: Double, B: Any?> eq_double_anyN(a: A, b: B) = a == b

fun <A: Double?, B: Double> eq_doubleN_double(a: A, b: B) = a == b

fun <A: Double?, B: Double?> eq_doubleN_doubleN(a: A, b: B) = a == b

fun <A: Double?, B: Any> eq_doubleN_any(a: A, b: B) = a == b

fun <A: Double?, B: Any?> eq_doubleN_anyN(a: A, b: B) = a == b

fun <A: Float, B: Float?> eq_float_floatN(a: A, b: B) = a == b

fun box(): String {
    if (!eq_double_doubleN(0.0, -0.0)) throw AssertionError("!eq_double_doubleN(0.0, -0.0)")
    if (eq_double_doubleN(0.0, null)) throw AssertionError("eq_double_doubleN(0.0, null)")
    if (!eq_double_any(0.0, 0.0)) throw AssertionError("!eq_double_any(0.0, 0.0)")
    if (eq_double_any(0.0, -0.0)) throw AssertionError("eq_double_any(0.0, -0.0)")
    if (!eq_double_anyN(0.0, 0.0)) throw AssertionError("!eq_double_anyN(0.0, 0.0)")
    if (eq_double_anyN(0.0, -0.0)) throw AssertionError("eq_double_anyN(0.0, -0.0)")
    if (eq_double_anyN(0.0, null)) throw AssertionError("eq_double_anyN(0.0, null)")

    if (eq_doubleN_double(null, 0.0)) throw AssertionError("eq_doubleN_double(null, 0.0)")
    if (!eq_doubleN_doubleN(0.0, -0.0)) throw AssertionError("!eq_doubleN_doubleN(0.0, -0.0)")
    if (eq_doubleN_doubleN(0.0, null)) throw AssertionError("eq_doubleN_doubleN(0.0, null)")
    if (!eq_doubleN_any(0.0, 0.0)) throw AssertionError("!eq_doubleN_any(0.0, 0.0)")
    if (eq_doubleN_any(0.0, -0.0)) throw AssertionError("eq_doubleN_any(0.0, -0.0)")
    if (!eq_doubleN_anyN(0.0, 0.0)) throw AssertionError("!eq_doubleN_anyN(0.0, 0.0)")
    if (eq_doubleN_anyN(0.0, -0.0)) throw AssertionError("eq_doubleN_anyN(0.0, -0.0)")
    if (eq_doubleN_anyN(0.0, null)) throw AssertionError("eq_doubleN_anyN(0.0, null)")
    if (!eq_float_floatN(0.0F, -0.0F)) throw AssertionError("!eq_float_floatN(0.0F, -0.0F)")
    if (eq_float_floatN(0.0F, null)) throw AssertionError("eq_float_floatN(0.0F, null)")

    return "OK"
}
"#,
        "NullableDoubleTypeParam",
    );
}
