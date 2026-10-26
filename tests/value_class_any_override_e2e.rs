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
