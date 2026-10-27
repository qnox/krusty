//! kotlinc states an elvis's own line where the elvis begins and again where its value is stored,
//! and keeps that entry when the left value's first instruction or the store marks another line at
//! the same offset.

use super::common;

#[test]
fn an_elvis_keeps_its_own_line_beside_its_operands_lines() {
    let source = "fun foo(a: Int): Int? = a\n\
         fun bar(a: Int): Int = a\n\
         \n\
         fun declared(a: Int): Int {\n\
         \x20   val y = foo(\n\
         \x20       a) ?: 0\n\
         \x20   return y\n\
         }\n\
         fun returned(a: Int): Int {\n\
         \x20   return foo(\n\
         \x20       a) ?: 0\n\
         }\n\
         fun body(a: Int): Int = foo(\n\
         \x20   a) ?: 0\n\
         fun assigned(a: Int): Int {\n\
         \x20   var y = 1\n\
         \x20   y = foo(\n\
         \x20       a) ?: 0\n\
         \x20   return y\n\
         }\n\
         fun argument(a: Int): Int {\n\
         \x20   return bar(foo(\n\
         \x20       a) ?: 0)\n\
         }\n\
         fun laterLeft(a: Int): Int {\n\
         \x20   val y =\n\
         \x20       foo(\n\
         \x20           a) ?: 0\n\
         \x20   return y\n\
         }\n\
         \n\
         fun box(): String {\n\
         \x20   val total = declared(1) + returned(2) + body(3) + assigned(4) + argument(5) +\n\
         \x20       laterLeft(6)\n\
         \x20   return if (total == 21) \"OK\" else \"fail $total\"\n\
         }\n\
         ";
    common::expect_box_ok_with_stdlib(source, "ElvisOwnLine");
    let classes = common::classes_against_kotlinc_module(&[("ElvisOwnLine.kt", source)]);
    let differences = classes.differences();
    assert!(
        differences.is_empty(),
        "classes differ from kotlinc's:\n\n{}",
        differences.join("\n\n")
    );
}
