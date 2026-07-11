//! A `when`/`if` branch body may be a LAMBDA (`when (x) { A -> { _ -> body } }`) when the expression
//! returns a function type — not just a statement block. The parser detects a top-level `->`. Runnable.
use super::common;
fn run(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, "Main")
}

#[test]
fn when_branch_lambda_body() {
    const SRC: &str = "var result = \"fail\"\n\
        fun mk(sel: Int): (Int) -> Unit = when (sel) {\n\
        \x20 1 -> { _ -> result = \"OK\" }\n\
        \x20 else -> { _ -> result = \"other\" }\n\
        }\n\
        fun box(): String {\n\
        \x20 mk(1)(0)\n\
        \x20 return result\n\
        }\n";
    assert_eq!(run(SRC).expect("when lambda branch"), "OK");
}

#[test]
fn if_branch_lambda_body() {
    const SRC: &str = "fun mk(b: Boolean): (String) -> String =\n\
        \x20 if (b) { s -> s + \"!\" } else { s -> s }\n\
        fun box(): String = if (mk(true)(\"O\") == \"O!\" && mk(false)(\"K\") == \"K\") \"OK\" else \"fail\"\n";
    assert_eq!(run(SRC).expect("if lambda branch"), "OK");
}
