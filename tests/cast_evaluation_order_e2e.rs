//! A non-null `as` narrows its operand for reads that run only after the cast.

use super::common::expect_box_same_as_kotlinc;

#[test]
fn cast_in_an_earlier_conjunct_narrows_an_inferred_equals() {
    expect_box_same_as_kotlinc(
        r#"
abstract class Version(val major: Int, val minor: Int, val patch: Int) {
    override fun equals(other: Any?) =
        other != null &&
            major == (other as Version).major &&
            minor == other.minor &&
            patch == other.patch
}

fun box(): String {
    val version = object : Version(1, 2, 3) {}
    return if (version.equals(version)) "OK" else "no"
}
"#,
        "CastConjunctEquals",
    );
}

#[test]
fn cast_on_the_left_of_an_operator_narrows_the_right_operand() {
    expect_box_same_as_kotlinc(
        r#"
fun same(other: Any): Boolean =
    (other as String).length == other.length

fun blank(other: Any): Boolean =
    (other as String).isEmpty() || other.isBlank()

fun widened(text: String) =
    (text as Any).hashCode() == (text as Any).hashCode() && text.isNotEmpty()

fun box(): String {
    if (!same("ab")) return "same"
    if (blank("ab") || !blank("")) return "blank"
    if (!widened("ab")) return "wide"
    return "OK"
}
"#,
        "CastOperatorOperand",
    );
}
