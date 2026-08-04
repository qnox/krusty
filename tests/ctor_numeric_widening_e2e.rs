//! An integer argument in a WIDER primitive constructor parameter (`C(b: Long)` called as `C(1)`).
//! Every other call origin admitted the widening the emit site materializes; constructor selection
//! measured arguments by SUBTYPING alone, so a constructor with a `Long`/`Double`/… parameter was
//! unreachable from an integer literal — "none of the following candidates is applicable". Covers the
//! module and the classpath origin, and the exact-overload preference that must survive the widening.
use super::common;

#[test]
fn a_module_constructor_accepts_an_integer_in_a_long_parameter() {
    let src = "class Row(val a: String, val b: Long, val c: Long)\n\
        fun box(): String {\n\
        \x20 val r = Row(\"x\", 1, 2)\n\
        \x20 return if (r.b == 1L && r.c == 2L) \"OK\" else \"b=${r.b} c=${r.c}\"\n\
        }\n";
    common::expect_box_ok_with_stdlib(src, "CtorWidenModule");
}

/// A same-arity `Int` overload is the BETTER match and must still win — the widening makes the `Long`
/// constructor applicable, and applicability alone would make the call ambiguous.
#[test]
fn an_exact_constructor_overload_still_wins_over_the_widened_one() {
    let src = "class Pick {\n\
        \x20 val tag: String\n\
        \x20 constructor(v: Int) { tag = \"int\" }\n\
        \x20 constructor(v: Long) { tag = \"long\" }\n\
        }\n\
        fun box(): String {\n\
        \x20 if (Pick(1).tag != \"int\") return \"int\"\n\
        \x20 if (Pick(1L).tag != \"long\") return \"long\"\n\
        \x20 return \"OK\"\n\
        }\n";
    common::expect_box_ok_with_stdlib(src, "CtorWidenOverload");
}

#[test]
fn a_classpath_constructor_accepts_an_integer_in_a_long_parameter() {
    const LIB: &str = "package lib\n\
        class Row(val a: String, val b: Long, val c: Long)\n";
    let main = "import lib.Row\n\
        fun box(): String {\n\
        \x20 val r = Row(\"x\", 1, 2)\n\
        \x20 return if (r.b == 1L && r.c == 2L) \"OK\" else \"b=${r.b} c=${r.c}\"\n\
        }\n";
    common::expect_box_ok_against("ctor_widen_classpath", LIB, main);
}
