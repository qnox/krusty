//! An enum class primary constructor may declare default parameter values
//! (`enum class C(val x: Int = 1)`); an entry that omits the argument gets the default. Before, the
//! enum constructor-parameter parser dropped the `= <default>`, failing with `expected ')'`. Runnable.
use super::common;

fn run(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, "Main")
}

#[test]
fn enum_entry_uses_default_argument() {
    const SRC: &str = "enum class Test(val str: String = \"OK\") {\n\
        \x20 OK\n\
        }\n\
        fun box(): String = Test.OK.str\n";
    assert_eq!(run(SRC).expect("enum default arg"), "OK");
}

#[test]
fn enum_mixed_default_and_explicit() {
    const SRC: &str = "enum class E(val a: Int, val b: Int = 10) {\n\
        \x20 X(1),\n\
        \x20 Y(2, 20)\n\
        }\n\
        fun box(): String {\n\
        \x20 val ok = E.X.a == 1 && E.X.b == 10 && E.Y.a == 2 && E.Y.b == 20\n\
        \x20 return if (ok) \"OK\" else \"fail\"\n\
        }\n";
    assert_eq!(run(SRC).expect("enum mixed default"), "OK");
}
