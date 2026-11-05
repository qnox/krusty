//! A value class's source `Any` overrides, against kotlinc.
//!
//! kotlinc realizes `override fun toString()` in a value class as the static `toString-impl` over
//! the carrier, then gives the box a `toString()` calling it. That box method is the override on
//! the box: it stands right after `toString-impl`, at the override's place among the declared
//! members, enters on the override's signature line and returns on its last line. krusty appended
//! it after every declared member, with no line.

use super::common;

const PLACEMENT: &str = r#"interface Described {
    fun describe(): String
}

@JvmInline
value class Text(val v: String) : Described {
    override fun toString(): String =
        "<" + v + ">"

    override fun describe(): String = "'" + v + "'"
}

fun box(): String {
    val text = Text("ab")
    val got = text.toString() + text.describe()
    return if (got == "<ab>'ab'") "OK" else got
}
"#;

#[test]
fn boxed_override_follows_its_static_impl() {
    common::assert_classes_identical_to_kotlinc("ValueClassAnyOverride", PLACEMENT, &["Text"]);
}

#[test]
fn boxed_override_runs() {
    common::expect_box_ok_with_stdlib(PLACEMENT, "ValueClassAnyOverride");
}

/// What an override lifts out of its body is named after the static `-impl` kotlinc renamed it to
/// first: a lambda in `toString` is `toString_impl$lambda$0`, as one in the interface override
/// `describe` is `describe_impl$lambda$0`. krusty named the interface override's lambda that way
/// but kept the source spelling for the `Any` override, `toString$lambda$0`.
const LIFTED: &str = r#"interface Described {
    fun describe(): String
}

@JvmInline
value class Text(val v: String) : Described {
    override fun toString(): String = lazy { v + "!" }.value

    override fun describe(): String = lazy { "'" + v + "'" }.value
}

fun box(): String {
    val text = Text("ab")
    val got = text.toString() + text.describe()
    return if (got == "ab!'ab'") "OK" else got
}
"#;

#[test]
fn override_lambda_is_named_after_its_static_impl() {
    common::assert_classes_identical_to_kotlinc("ValueClassAnyOverrideLambda", LIFTED, &["Text"]);
}

#[test]
fn override_lambda_runs() {
    common::expect_box_ok_with_stdlib(LIFTED, "ValueClassAnyOverrideLambda");
}

/// A value class's source `equals` override, against kotlinc.
///
/// kotlinc realizes the override as the static `equals-impl` over the carrier and gives the box an
/// `equals(Object)` calling it. That box method is the override, so its parameter keeps
/// `Any.equals`'s `other: Any?` and is annotated `@Nullable`; krusty annotated it `@NotNull`. The
/// specialized `equals-impl0` compares two carriers through the override, on their boxes:
/// `box-impl(p1).equals(box-impl(p2))`. krusty called `equals-impl(p1, box-impl(p2))`.
const CUSTOM_EQUALS: &str = r#"@file:Suppress("RESERVED_MEMBER_INSIDE_VALUE_CLASS")

@JvmInline
value class Z(val data: Int) {
    override fun equals(other: Any?): Boolean =
        other is Z &&
                data % 256 == other.data % 256
}

@JvmInline
value class T(val text: String) {
    override fun equals(other: Any?): Boolean {
        if (other !is T) return false
        return text.length == other.text.length
    }

    override fun hashCode(): Int = text.length
}

fun box(): String {
    if (Z(0) != Z(256)) return "Z"
    if (T("ab") != T("cd") || T("a") == T("ab")) return "T"
    return "OK"
}
"#;

#[test]
fn custom_equals_matches_kotlinc() {
    common::assert_classes_identical_to_kotlinc(
        "ValueClassCustomEquals",
        CUSTOM_EQUALS,
        &["Z", "T"],
    );
}

#[test]
fn custom_equals_runs() {
    common::expect_box_ok_with_stdlib(CUSTOM_EQUALS, "ValueClassCustomEquals");
}
