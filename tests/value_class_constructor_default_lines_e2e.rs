//! Where a value class's `constructor-impl$default` maps its lines, against kotlinc.
//!
//! A value class's constructor is realized as the static `constructor-impl`, and an omitted
//! argument is filled by its `constructor-impl$default(…, int, DefaultConstructorMarker)`. kotlinc
//! builds that stub like any function's: its entry and the closing call stand on the constructor's
//! own line (the primary's `constructor` keyword or its parameter list's `(`, a secondary's
//! `constructor` keyword), and each masked fill on its default expression's line.
//! `constructor-impl` itself records no line.
//!
//! krusty emitted the primary's stub with no `LineNumberTable` at all, and a secondary's without
//! its default expressions' lines.

use super::common;

/// The fixture's line numbers are part of what kotlinc's class files record.
const SOURCE: &str = r#"@JvmInline
value class SameLine(val v: Int = 7)

@JvmInline
value class OwnLines(
    val v: Int =
        "abc".length
)

@JvmInline
value class Secondary(val v: Int) {
    constructor(
        a: Int,
        b: Int =
            "xy".length
    ) : this(a + b)
}

fun box(): String {
    val got = "" + SameLine().v + OwnLines().v + Secondary(a = 1).v
    return if (got == "733") "OK" else got
}
"#;

#[test]
fn the_default_stub_maps_the_constructor_and_its_defaults() {
    common::assert_classes_identical_to_kotlinc(
        "ConstructorDefaultLines",
        SOURCE,
        &["SameLine", "OwnLines", "Secondary"],
    );
}

#[test]
fn omitted_constructor_arguments_take_their_defaults() {
    common::expect_box_ok_with_stdlib(SOURCE, "ConstructorDefaultLines");
}
