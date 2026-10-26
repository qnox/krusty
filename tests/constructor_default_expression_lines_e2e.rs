//! The lines of a constructor's `$default` overload while it evaluates a default, against kotlinc.
//!
//! kotlinc generates the overload's masked fills like any other code: each default expression maps
//! to its own lines, nested calls included, and the mask tests between them to the constructor's
//! line. krusty set the primary constructor's whole table by hand, putting each fill on the line
//! of the class field at the parameter's index (another parameter's line once a parameter is no
//! property), and a secondary constructor's on its default's first line only.

use super::common;

const SOURCE: &str = r#"fun k(s: String) = s

class Holder(
    val v: String =
        "O" +
            k(
                "K"
            ),
    w: Int =
        2,
    val z: Int = 1
) {
    constructor(
        a: Long,
        b: String =
            k(
                "q"
            ),
    ) : this(b + a)

    val q = w + z
}

fun box(): String = if (Holder(1L).v == "q1" && Holder().q == 3) Holder().v else "fail"
"#;

#[test]
fn default_fills_map_to_their_expressions_lines() {
    common::assert_classes_identical_to_kotlinc("ConstructorDefaultLines", SOURCE, &["Holder"]);
}

#[test]
fn default_fills_run() {
    common::expect_box_ok_with_stdlib(SOURCE, "ConstructorDefaultLines");
}
