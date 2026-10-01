//! A nullable function value adapted to a nullable fun interface stays null.
//!
//! Wrapping the null produces a non-null SAM whose method throws. A non-null function and a
//! lambda literal still become the interface.

use super::common;

const SRC: &str = r#"
fun interface KRunnable {
    fun invoke()
}

fun isNull(r: KRunnable?): Boolean {
    if (r == null) return true
    r.invoke()
    return false
}

fun nullableFun(fromNull: Boolean): (() -> Unit)? =
    if (fromNull) null else {{}}

fun box(): String {
    if (!isNull(nullableFun(true))) return "Fail 1"
    if (isNull(nullableFun(false))) return "Fail 2"
    if (!isNull(null)) return "Fail 3"
    if (isNull {}) return "Fail 4"
    return "OK"
}
"#;

#[test]
fn a_nullable_function_value_stays_null_as_a_fun_interface() {
    common::expect_box_same_as_kotlinc(SRC, "NullableSam");
}

#[test]
fn a_nullable_function_value_stays_null_under_class_sam_conversion() {
    common::expect_box_same_as_kotlinc_with_args(
        SRC,
        "NullableSamClass",
        &["-Xlambdas=class", "-Xsam-conversions=class"],
    );
}
