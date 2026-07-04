//! Backtick-quoted identifiers (`` `in` ``, `` `is` ``, `` `name with spaces` ``) — Kotlin's escape for
//! using a keyword (or otherwise-illegal name) as an identifier. The lexer reads the content between the
//! backticks as a plain `Ident` (never re-mapped to a keyword). Round-tripped on the JVM.

use super::common;

fn run(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, "Main")
}

#[test]
fn backtick_keyword_parameter_and_local() {
    const SRC: &str = "fun f(`in`: String): String {\n\
    val `is` = `in` + \"K\"\n\
    return `is`\n\
}\n\
fun box(): String = f(\"O\")\n";
    assert_eq!(run(SRC).expect("backtick keyword idents"), "OK");
}

#[test]
fn backtick_constructor_keyword_param() {
    const SRC: &str = "class A(val x: String) {\n\
    constructor(`in`: String, y: String) : this(`in` + y)\n\
}\n\
fun box(): String = A(\"O\", \"K\").x\n";
    assert_eq!(run(SRC).expect("backtick ctor param"), "OK");
}

#[test]
fn backtick_function_name_with_spaces() {
    const SRC: &str = "fun `make result`(): String = \"OK\"\n\
fun box(): String = `make result`()\n";
    assert_eq!(run(SRC).expect("backtick fn name with spaces"), "OK");
}
