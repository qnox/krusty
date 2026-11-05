//! A private value-class constructor used from another class of the same declaration.
//!
//! A value class's constructor is the static `constructor-impl` over its carrier, as private as the
//! declaration it realizes. The companion, a nested class and an anonymous object are other JVM
//! classes, so they reach it through the box's synthetic `access$constructor-impl`, kotlinc's
//! accessor for any private static member; the class's own code calls it directly. A lambda in the
//! companion is a method of the companion and takes the accessor too, also where its result is
//! boxed around the call. A private secondary constructor is its own `constructor-impl` and is
//! reached the same way.

use super::common;

const SOURCE: &str = r#"@JvmInline value class Meters private constructor(val v: Int) {
    private constructor(a: Int, b: Int) : this(a + b)

    companion object {
        fun of(v: Int): Meters = Meters(v)
        fun sum(a: Int, b: Int): Meters = Meters(a, b)
        fun later(v: Int): () -> Meters = { Meters(v * 2) }
    }

    class Maker {
        fun make(v: Int): Meters = Meters(v)
    }

    fun described(): Any = object {
        override fun toString(): String = Meters(v + 1).v.toString()
    }
}

fun box(): String {
    val got = "" + Meters.of(3).v + Meters.sum(1, 2).v + Meters.later(4)().v +
        Meters.Maker().make(5).v + Meters.of(6).described()
    return if (got == "33857") "OK" else got
}
"#;

#[test]
fn the_constructor_is_reached_through_kotlincs_accessor() {
    common::assert_classes_identical_to_kotlinc(
        "PrivateConstructorAccess",
        SOURCE,
        &[
            "Meters",
            "Meters$Companion",
            "Meters$Maker",
            "Meters$described$1",
        ],
    );
}

#[test]
fn every_scope_constructs_the_value() {
    common::expect_box_ok_with_stdlib(SOURCE, "PrivateConstructorAccess");
}

/// A private constructor over a value class is not hidden behind the marker overload: kotlinc
/// calls the erased `<init>` from the class's own code, keeps the marker one only as the accessor a
/// nested class reaches it through, and names the erased one in `@Metadata`.
const PRIVATE_OUTER_CONSTRUCTOR: &str = r#"@JvmInline
value class S(val string: String)

class Outer private constructor(val s: S) {
    class Nested {
        fun test(s: S) = Outer(s)
    }
}

fun box() = Outer.Nested().test(S("OK")).s.string
"#;

#[test]
fn a_private_constructor_over_a_value_class_names_its_erased_init() {
    common::assert_classes_identical_to_kotlinc(
        "PrivateOuterConstructor",
        PRIVATE_OUTER_CONSTRUCTOR,
        &["Outer", "Outer$Nested"],
    );
}

#[test]
fn a_private_constructor_over_a_value_class_runs() {
    common::expect_box_ok_with_stdlib(PRIVATE_OUTER_CONSTRUCTOR, "PrivateOuterConstructor");
}
