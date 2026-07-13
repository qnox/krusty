//! Constructing an `inner class` from inside the enclosing class (`Inner()`), which captures the
//! enclosing instance (`this$0`) so the inner body reads the outer's members. Same-file, runnable.
use super::common;
fn run(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, "Main")
}

#[test]
fn inner_class_construct_and_read_outer() {
    const SRC: &str = "class A(val x: Int) {\n\
        \x20 fun getx() = x + 1\n\
        \x20 inner class Inner {\n\
        \x20   fun r(): Int = x + getx()\n\
        \x20 }\n\
        \x20 fun make(): Inner = Inner()\n\
        }\n\
        fun box(): String = if (A(7).make().r() == 15) \"OK\" else \"no\"\n";
    assert_eq!(run(SRC).expect("inner class construct"), "OK");
}

#[test]
fn inner_class_with_ctor_args() {
    const SRC: &str = "class A(val base: Int) {\n\
        \x20 inner class Inner(val add: Int) { fun r(): Int = base + add }\n\
        \x20 fun make(a: Int): Inner = Inner(a)\n\
        }\n\
        fun box(): String = if (A(10).make(5).r() == 15) \"OK\" else \"no\"\n";
    assert_eq!(run(SRC).expect("inner class ctor args"), "OK");
}
