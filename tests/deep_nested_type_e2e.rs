//! A type nested two-or-more levels deep (`A { B { C } }`) must hoist to the FULL path `A.B.C`
//! (internal `A$B$C`) — not the truncated `B.C` the immediate-parent prefix alone produces — so a
//! member of `B` can reference `C` by simple name (Kotlin's nested-type scoping). Previously krusty
//! REJECTED `val c: C` inside a nested `B` as unresolved, dropping the whole file (the dominant
//! RED_REJECTED bucket on generated models). This asserts the file now compiles + runs.
use super::common;

fn run(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, "Main")
}

#[test]
fn nested_class_resolves_own_nested_type() {
    // `B` (nested in `A`) references its own nested `C` by simple name in a field type — previously an
    // "unresolved reference 'C'" rejection. The whole file must now compile (and `box` run).
    const SRC: &str = "class A {\n\
        \x20 data class B(val c: C, val n: Int) {\n\
        \x20   data class C(val x: Int)\n\
        \x20 }\n\
        }\n\
        fun box(): String = \"OK\"\n";
    assert_eq!(run(SRC).expect("deep nested-type file compiles"), "OK");
}

#[test]
fn a_dollar_in_a_nested_name_is_part_of_the_name() {
    // `$` is an ordinary character of a backticked name, not a nesting separator: `Dollar$Point`
    // and `Point` are distinct members of `Outer` (`Outer$Dollar$Point` and `Outer$Point`).
    const SRC: &str = "class Outer {\n\
        \x20 class `Dollar$Point`(val x: Int)\n\
        \x20 class Point(val y: Int)\n\
        }\n\
        fun box(): String = if (Outer.`Dollar$Point`(1).x + Outer.Point(2).y == 3) \"OK\" else \"fail\"\n";
    common::expect_box_same_as_kotlinc(SRC, "NestedDollarName");
}
