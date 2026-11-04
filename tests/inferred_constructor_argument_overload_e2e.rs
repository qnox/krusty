//! A library constructor whose type arguments are inferred from its own value arguments is argument
//! evidence for an overloaded callee: `Pair(1, 2)` is `Pair<Int, Int>`, so it selects `pick(Any?)`
//! rather than `pick(Int)`. Its result template once omitted the constructor's parameters, so the
//! call looked result-only and fit every overload: `pick(Pair(1, 2))` committed `pick(Int)` and
//! reported an argument type mismatch, and `println(Pair("x", 2))` found no applicable candidate.
//!
//! The source-class form is the portable box regression
//! `tests/box_regressions/inferredConstructorArgumentOverload.kt`. This one stays on the JVM
//! because the KLIB backends do not yet construct `kotlin.Pair` or `kotlin.Triple`.
use super::common;

#[test]
fn an_inferred_library_constructor_selects_the_any_overload() {
    const SOURCE: &str = "fun pick(value: Any?): String = \"any\"\n\
        fun pick(value: Int): String = \"int\"\n\
        fun pick(value: String): String = \"string\"\n\
        \n\
        fun box(): String {\n\
        \x20   val result = pick(Pair(1, 2)) + \" \" + pick(Triple(1, 2, 3)) + \" \" + pick(Pair(\"x\", 'c'))\n\
        \x20   return if (result == \"any any any\") \"OK\" else \"fail: $result\"\n\
        }\n";
    common::expect_box_same_as_kotlinc(SOURCE, "inferred_library_constructor_overload");
}
