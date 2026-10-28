//! A data class may declare its own overload of a `componentN` name. The compiler-generated
//! `componentN()` joins that name's overloads rather than replacing them, so a call with the
//! written overload's arguments selects it, and destructuring still reads the generated one.

use super::common;

const SOURCE: &str = "package demo\n\
                      data class Pair2(val left: Int, val right: String) {\n\
                      \x20   fun component1(scale: Int): Int = left * scale\n\
                      \x20   fun component2(suffix: String): String = right + suffix\n\
                      }\n\
                      fun box(): String {\n\
                      \x20   val pair = Pair2(2, \"O\")\n\
                      \x20   if (pair.component1(3) != 6) return \"written component1\"\n\
                      \x20   if (pair.component1() != 2) return \"generated component1\"\n\
                      \x20   val (left, right) = pair\n\
                      \x20   if (left != 2) return \"destructured left\"\n\
                      \x20   return pair.component2(\"K\").substring(right.length - 1)\n\
                      }\n";

#[test]
fn a_written_component_overload_keeps_the_generated_component() {
    common::expect_box_ok_with_stdlib(SOURCE, "DataClassComponentOverload");
    let pair = common::ModuleClassPair::compile(&[("Pair2.kt", SOURCE)], "demo/Pair2");
    assert!(
        pair.krusty == pair.kotlinc,
        "demo/Pair2 differs from kotlinc's"
    );
}
