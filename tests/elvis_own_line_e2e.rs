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

/// kotlinc folds `a ?: b ?: c` into one chain. On each link's null path it marks the next link's
/// operand line at the `pop`, before that operand's first instruction, and every later link's
/// temporary restates the line the chain started on where it is stored. The last link's right side
/// keeps the ordinary elvis layout.
#[test]
fn an_elvis_chain_marks_each_link_like_kotlinc() {
    let source = "fun pick(x: String): String? = if (x.length > 1) x else null\n\
         \n\
         fun two(x: String, y: String, z: String): String =\n\
         \x20   pick(x)\n\
         \x20       ?: pick(y)\n\
         \x20       ?: z\n\
         fun three(x: String, y: String, z: String, w: String): String =\n\
         \x20   pick(x)\n\
         \x20       ?: pick(y)\n\
         \x20       ?: pick(z)\n\
         \x20       ?: w\n\
         fun sameLine(x: String, y: String, z: String): String =\n\
         \x20   pick(x) ?: pick(y)\n\
         \x20       ?: z\n\
         fun declared(x: String, y: String, z: String): String {\n\
         \x20   val v = pick(x)\n\
         \x20       ?: pick(y)\n\
         \x20       ?: z\n\
         \x20   return v\n\
         }\n\
         fun nullable(x: String, y: String, z: String): String? =\n\
         \x20   pick(x)\n\
         \x20       ?: pick(y)\n\
         \x20       ?: pick(z)\n\
         \n\
         fun box(): String {\n\
         \x20   val result = two(\"a\", \"bb\", \"c\") + three(\"a\", \"b\", \"c\", \"d\") +\n\
         \x20       sameLine(\"aa\", \"b\", \"c\") + declared(\"a\", \"b\", \"cc\") +\n\
         \x20       (nullable(\"a\", \"b\", \"c\") ?: \"-\")\n\
         \x20   return if (result == \"bbdaacc-\") \"OK\" else \"fail $result\"\n\
         }\n\
         ";
    common::expect_box_ok_with_stdlib(source, "ElvisChainLines");
    let classes = common::classes_against_kotlinc_module(&[("ElvisChainLines.kt", source)]);
    let differences = classes.differences();
    assert!(
        differences.is_empty(),
        "classes differ from kotlinc's:\n\n{}",
        differences.join("\n\n")
    );
}
