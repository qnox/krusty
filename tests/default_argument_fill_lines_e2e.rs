//! The line a function's `$default` stub opens a masked fill on, against kotlinc.
//!
//! kotlinc stores the default into its parameter as a statement at the default expression's
//! position: the fill opens on the expression's first line, and when the value's first operand
//! marks another line at that same offset (a call whose argument starts on the next line) the
//! table keeps both entries. krusty kept only the operand's.

use super::common;

const SOURCE: &str = r#"fun k(s: String) = s

fun f(
    b: String =
        k(
            "q"
        ),
): String = b

class C {
    fun g(
        b: String =
            k(
                "q"
            ),
    ): String = b
}

fun box(): String = if (f() == "q" && C().g() == "q") "OK" else "fail"
"#;

#[test]
fn fill_opens_on_the_default_expression() {
    common::assert_classes_identical_to_kotlinc(
        "DefaultFillLines",
        SOURCE,
        &["DefaultFillLinesKt", "C"],
    );
}

#[test]
fn fill_runs() {
    common::expect_box_ok_with_stdlib(SOURCE, "DefaultFillLines");
}
