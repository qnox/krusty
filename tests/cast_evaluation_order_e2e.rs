//! A non-null `as` narrows its operand for reads that run only after the cast.

use super::common::{assert_errors_match_kotlinc, expect_box_same_as_kotlinc};

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
class Payload(val value: Int)

fun same(other: Any): Boolean =
    (other as Payload).value == other.value

fun either(other: Any): Boolean =
    (other as Payload).value == 0 || other.value == 2

fun widened(payload: Payload) =
    (payload as Any) == payload && payload.value == 2

fun box(): String {
    if (!same(Payload(2))) return "same"
    if (!either(Payload(0)) || !either(Payload(2))) return "either"
    if (!widened(Payload(2))) return "wide"
    return "OK"
}
"#,
        "CastOperatorOperand",
    );
}

#[test]
fn cast_only_on_the_right_of_and_does_not_narrow_a_later_read() {
    assert_errors_match_kotlinc(
        &[(
            "Main.kt",
            r#"
class Payload(val value: Int)
fun later(other: Any): Int {
    if (false && (other as Payload).value == 0) {}
    return other.value
}
"#,
        )],
        &[],
    );
}

#[test]
fn cast_only_on_the_right_of_or_does_not_narrow_a_later_read() {
    assert_errors_match_kotlinc(
        &[(
            "Main.kt",
            r#"
class Payload(val value: Int)
fun later(other: Any): Int {
    if (true || (other as Payload).value == 0) {}
    return other.value
}
"#,
        )],
        &[],
    );
}

#[test]
fn cast_in_a_completed_statement_narrows_the_following_statement() {
    expect_box_same_as_kotlinc(
        r#"
class Payload(val value: Int)

fun read(other: Any): Int {
    other as Payload
    return other.value
}

fun box(): String = if (read(Payload(7)) == 7) "OK" else "statement"
"#,
        "CastCompletedStatement",
    );
}

#[test]
fn casts_nested_in_normal_completion_forms_narrow_later_reads() {
    expect_box_same_as_kotlinc(
        r#"
class Payload(var value: Int) {
    fun read(): Int = value
}

val extensionRead: Payload.() -> Int = { value }

fun template(other: Any): Int = "${other as Payload}".length + other.value
fun extensionAccess(other: Any): Int = (other as Payload).(extensionRead)() + other.value
fun boundReference(other: Any): Int = ((other as Payload)::read)() + other.value
fun whenSubject(other: Any): Int = when (other as Payload) { else -> 1 } + other.value

fun box(): String {
    if (template(Payload(2)) <= 2) return "template"
    if (extensionAccess(Payload(2)) != 4) return "extension"
    if (boundReference(Payload(2)) != 4) return "reference"
    if (whenSubject(Payload(2)) != 3) return "when"
    return "OK"
}
"#,
        "CastNestedNormalCompletion",
    );
}
