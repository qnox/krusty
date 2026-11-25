//! An adapted reference to an inline function inlines the callee into the adapter's `invoke`; after
//! the inlined body kotlinc re-enters the reference's line before the result is returned.

use super::common;

#[test]
fn an_inline_adapted_reference_restores_its_line_after_the_inlined_body() {
    let classes = common::classes_against_kotlinc_module(&[
        (
            "lib.kt",
            "package lib\n\
             \n\
             inline fun inlineFoo1(a: Int, vararg b: String, c: Int = 0) = 0\n\
             inline fun Int.inlineFoo2(vararg b: String, c: Int = 0) = 0\n",
        ),
        (
            "main.kt",
            "package main\n\
             \n\
             import lib.inlineFoo1\n\
             import lib.inlineFoo2\n\
             \n\
             fun unitConversion(ref: Int.(Array<String>, Int) -> Unit) = ref\n\
             \n\
             fun testInline() {\n\
             \x20   unitConversion(::inlineFoo1)\n\
             \x20   unitConversion(Int::inlineFoo2)\n\
             }\n",
        ),
    ]);
    let differences = classes.differences();
    assert!(
        differences.is_empty(),
        "classes differ from kotlinc's:\n\n{}",
        differences.join("\n\n")
    );
}
