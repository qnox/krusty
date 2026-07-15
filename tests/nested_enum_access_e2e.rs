//! A nested `enum class` declared in a class body is hoisted to a top-level `Outer.Inner` type
//! (internal `Outer$Inner`) and its entries are read through the enclosing type name
//! (`Outer.Inner.ENTRY`). The checker types the qualified access as the enum's own type and the
//! lowerer emits the enum-constant read.

use crate::common;

fn check(rel: &str) {
    match common::run_box_corpus_case(rel) {
        Some(s) => assert_eq!(s, "OK", "{rel}"),
        None => panic!("unexpectedly skipped: {rel}"),
    }
}

#[test]
fn nested_enum_entry_via_outer_name() {
    check("enum/inner.kt");
}

#[test]
fn nested_enum_with_body_and_value() {
    // A nested enum with a constructor and a member, read through the outer name.
    let src = r#"
class Palette {
    enum class Color(val hex: Int) {
        RED(0xff0000),
        GREEN(0x00ff00)
    }
}

fun box(): String {
    val c = Palette.Color.GREEN
    return if (c.hex == 0x00ff00 && c.name == "GREEN") "OK" else "FAIL"
}
"#;
    common::expect_box_ok_with_stdlib(src, "NestedEnumBody");
}
