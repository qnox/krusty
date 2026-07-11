//! An enum class may declare body member properties (`enum class E(val a: Int) { X(3); val b = a*2 }`)
//! — a backing field on the enum class initialized in its constructor. The parser accepts a `val`/`var`
//! body member, and the enum emitter declares the field and runs its initializer in the constructor.
//! Same-file, runnable.
use super::common;

fn run(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, "Main")
}

#[test]
fn enum_body_property_reads_ctor_param() {
    const SRC: &str = "enum class E(val a: Int) {\n\
        \x20 X(3),\n\
        \x20 Y(5);\n\
        \x20 val b = a * 2\n\
        }\n\
        fun box(): String =\n\
        \x20 if (E.X.b == 6 && E.Y.b == 10) \"OK\" else \"fail\"\n";
    assert_eq!(run(SRC).expect("enum body property"), "OK");
}

#[test]
fn enum_body_const_property() {
    const SRC: &str = "enum class E {\n\
        \x20 A, B;\n\
        \x20 val b = 42\n\
        }\n\
        fun box(): String = if (E.A.b == 42 && E.B.b == 42) \"OK\" else \"fail\"\n";
    assert_eq!(run(SRC).expect("enum body const property"), "OK");
}

#[test]
fn enum_body_var_property() {
    const SRC: &str = "enum class E {\n\
        \x20 A, B;\n\
        \x20 var hits = 0\n\
        }\n\
        fun box(): String {\n\
        \x20 E.A.hits = 7\n\
        \x20 return if (E.A.hits == 7 && E.B.hits == 0) \"OK\" else \"fail\"\n\
        }\n";
    assert_eq!(run(SRC).expect("enum body var property"), "OK");
}
