//! A super-constructor delegation may use NAMED (reordered) arguments
//! (`class D : Base(name = …, addr = …)`); they are reordered to the base constructor's parameter
//! order. Runnable.
use super::common;
fn run(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, "Main")
}

#[test]
fn named_reordered_super_args() {
    const SRC: &str = "open class Base(val addr: Long, val name: String)\n\
        class D : Base(name = \"OK\", addr = 4660L)\n\
        fun box(): String =\n\
        \x20 if (D().addr == 4660L && D().name == \"OK\") \"OK\" else \"fail\"\n";
    assert_eq!(run(SRC).expect("named super args"), "OK");
}
