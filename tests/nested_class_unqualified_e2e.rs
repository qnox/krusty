//! An unqualified reference to a SIBLING nested class inside the enclosing class body
//! (`Inner()` in `class Outer { class Inner { … } }`) resolves to `Outer$Inner` — Kotlin's
//! nested-class scoping (a qualified `Outer.Inner()` already worked).

mod common;

fn run(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, "Main")
}

#[test]
fn unqualified_nested_class_construction() {
    const SRC: &str = "class Outer {\n\
    class Inner { fun v() = \"OK\" }\n\
    fun make(): String = Inner().v()\n\
}\n\
fun box(): String = Outer().make()\n";
    assert_eq!(run(SRC).expect("unqualified nested class"), "OK");
}

#[test]
fn unqualified_nested_class_with_ctor_arg() {
    const SRC: &str = "class Outer {\n\
    class Inner(val s: String) { fun v() = s }\n\
    fun make(): String = Inner(\"OK\").v()\n\
}\n\
fun box(): String = Outer().make()\n";
    assert_eq!(run(SRC).expect("unqualified nested class with arg"), "OK");
}
