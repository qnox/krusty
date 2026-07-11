//! A `vararg` primary-constructor parameter on an enum class (`enum class E(vararg val xs: Int)`) is
//! exposed as an array; an entry that supplies no arguments constructs an empty array. Runnable.
use super::common;
fn run(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, "Main")
}

#[test]
fn enum_vararg_val_empty() {
    const SRC: &str = "enum class E(vararg val xs: Int) {\n\
        \x20 A;\n\
        }\n\
        fun box(): String = if (E.A.xs.size == 0) \"OK\" else \"fail\"\n";
    assert_eq!(run(SRC).expect("enum vararg empty"), "OK");
}
