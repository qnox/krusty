//! An `interface` may declare a `companion object` with `val` properties; they are emitted as static
//! fields on the interface and read as `C.X`. Runnable.
use super::common;
fn run(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, "Main")
}

#[test]
fn interface_companion_const_val() {
    const SRC: &str = "interface C {\n\
        \x20 companion object { const val FOO: String = \"OK\" }\n\
        }\n\
        fun box(): String = C.FOO\n";
    assert_eq!(run(SRC).expect("interface companion const"), "OK");
}

#[test]
fn interface_companion_non_const_val() {
    const SRC: &str = "interface C {\n\
        \x20 companion object { val FOO: String = \"O\" + \"K\" }\n\
        }\n\
        fun box(): String = C.FOO\n";
    assert_eq!(run(SRC).expect("interface companion non-const"), "OK");
}

#[test]
fn interface_companion_method() {
    const SRC: &str = "interface C {\n\
        \x20 companion object { fun make(): String = \"OK\" }\n\
        }\n\
        fun box(): String = C.make()\n";
    assert_eq!(run(SRC).expect("interface companion method"), "OK");
}

#[test]
fn interface_companion_method_and_prop() {
    const SRC: &str = "interface C {\n\
        \x20 companion object {\n\
        \x20   val P: String = \"O\" + \"K\"\n\
        \x20   fun make(): String = \"OK\"\n\
        \x20 }\n\
        }\n\
        fun box(): String = if (C.P == \"OK\" && C.make() == \"OK\") \"OK\" else \"fail\"\n";
    assert_eq!(run(SRC).expect("interface companion method+prop"), "OK");
}
