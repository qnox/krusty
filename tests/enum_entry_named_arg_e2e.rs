//! An enum entry may construct with NAMED and reordered constructor arguments
//! (`A(b = 0, a = 1)`), combined with defaults. Before, the entry-argument parser only accepted
//! positional expressions, failing with `expected ')'` at the `=`. Runnable.
use super::common;

fn run(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, "Main")
}

#[test]
fn enum_entry_named_reordered_args() {
    const SRC: &str = "enum class E(val a: Int, val b: Int) {\n\
        \x20 X(b = 0, a = 1)\n\
        }\n\
        fun box(): String =\n\
        \x20 if (E.X.a == 1 && E.X.b == 0) \"OK\" else \"fail\"\n";
    assert_eq!(run(SRC).expect("enum named reordered args"), "OK");
}

#[test]
fn enum_entry_named_arg_with_default() {
    const SRC: &str = "enum class E(val a: Int, val b: Int = 10) {\n\
        \x20 X(a = 5),\n\
        \x20 Y(b = 20, a = 6)\n\
        }\n\
        fun box(): String {\n\
        \x20 val ok = E.X.a == 5 && E.X.b == 10 && E.Y.a == 6 && E.Y.b == 20\n\
        \x20 return if (ok) \"OK\" else \"fail\"\n\
        }\n";
    assert_eq!(run(SRC).expect("enum named arg with default"), "OK");
}
