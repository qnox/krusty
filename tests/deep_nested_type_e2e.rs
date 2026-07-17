//! A type nested two-or-more levels deep (`A { B { C } }`) must hoist to the FULL path `A.B.C`
//! (internal `A$B$C`) — not the truncated `B.C` the immediate-parent prefix alone produces — so a
//! member of `B` can reference `C` by simple name (Kotlin's nested-type scoping). Previously krusty
//! REJECTED `val c: C` inside a nested `B` as unresolved, dropping the whole file (the dominant
//! RED_REJECTED bucket on the generated httpclient models). This asserts the file now compiles + runs.
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
