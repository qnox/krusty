//! kotlinc's test corpus wraps expressions/declarations in inline diagnostic markers
//! `<!DIAGNOSTIC_NAME!>expr<!>` (and the bare close `<!>`). The lexer strips them as trivia (real
//! Kotlin never has `<!` adjacent), so a marker-annotated source compiles. Round-tripped on the JVM.

use super::common;

fn run(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, "Main")
}

#[test]
fn markers_around_expression() {
    const SRC: &str = "fun box(): String {\n\
    val x = <!SOME_DIAGNOSTIC!>\"OK\"<!>\n\
    return x\n\
}\n";
    assert_eq!(run(SRC).expect("markers stripped"), "OK");
}

#[test]
fn markers_with_comma_names_and_nested() {
    // Comma-separated diagnostic names and a marker around a sub-expression.
    const SRC: &str = "fun id(s: String) = s\n\
fun box(): String =\n\
    id(<!A, B!>\"O\"<!>) + <!C!>id(\"K\")<!>\n";
    assert_eq!(run(SRC).expect("comma/nested markers stripped"), "OK");
}
