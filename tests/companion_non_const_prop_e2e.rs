//! A `companion object` may declare a non-const `val` property; it is emitted as a static field on the
//! outer class, initialized in the outer class's `<clinit>`, and read as `getstatic C.X`. Runnable.
use super::common;
fn run(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, "Main")
}

#[test]
fn companion_non_const_val() {
    const SRC: &str = "class C {\n\
        \x20 companion object { val FOO: String = \"O\" + \"K\" }\n\
        }\n\
        fun box(): String = C.FOO\n";
    assert_eq!(run(SRC).expect("companion non-const val"), "OK");
}
