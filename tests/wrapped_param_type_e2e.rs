//! A constructor/property parameter whose TYPE is on the next line (a wrapped declaration the code
//! generator emits for long names: `val x:\n    Type`) must parse — Kotlin allows a newline after the
//! `:` of a type annotation. krusty's parser required the type immediately after the colon and rejected
//! the whole file (a RED_REJECTED bucket on the generated httpclient models).
use super::common;

#[test]
fn param_type_on_next_line_parses() {
    const SRC: &str = "data class D(\n\
        \x20 val first:\n\
        \x20   Int,\n\
        \x20 val second: String,\n\
        )\n\
        fun box(): String = if (D(7, \"a\").first == 7) \"OK\" else \"FAIL\"\n";
    assert_eq!(
        common::compile_and_run_with_stdlib(SRC, "Main").expect("wrapped param type"),
        "OK"
    );
}
