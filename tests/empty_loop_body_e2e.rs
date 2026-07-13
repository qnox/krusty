//! An empty loop body written as a bare `;` (`while (c);`, `for (…);`). The `;` is an explicit empty
//! body — kotlinc runs the loop for its side effects. Previously the `;` (lexed like a newline) was
//! skipped and the FOLLOWING statement was mistaken for the body. Same-file, runnable.
use super::common;
fn run(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, "Main")
}

#[test]
fn empty_while_body() {
    const SRC: &str = "fun box(): String {\n\
        \x20 var x = 0\n\
        \x20 while (x++ < 5);\n\
        \x20 return if (x == 6) \"OK\" else \"no:\" + x\n\
        }\n";
    assert_eq!(run(SRC).expect("empty while"), "OK");
}

#[test]
fn empty_for_body() {
    const SRC: &str = "fun box(): String {\n\
        \x20 var s = 0\n\
        \x20 for (i in 1..4) s += i\n\
        \x20 var t = 0\n\
        \x20 for (i in 1..4);\n\
        \x20 return if (s == 10 && t == 0) \"OK\" else \"no\"\n\
        }\n";
    assert_eq!(run(SRC).expect("empty for"), "OK");
}
