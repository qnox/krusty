//! A syntax error drops the file from header inventory. Inspection still opens the file to
//! discover anonymous-object captures; the checker used to panic with "checked class has a
//! collected signature" on the object expression inside `g`.

use super::common;

const SOURCE: &str =
    "class C {\n    fun g(): Int = object { fun f() = 1 }.f()\n    fun broken(\n}\n";

#[test]
fn a_syntax_error_beside_an_anonymous_object_matches_kotlinc() {
    let mut diagnostics = krusty::diag::DiagSink::new();
    let (_file, _symbols, _info) =
        krusty::frontend::analyze_source_standalone(SOURCE, &mut diagnostics);
    let rendered = diagnostics.render("Broken.kt", SOURCE);
    let (_code, reference_stderr) = common::reference_compiler_run(&[("Broken.kt", SOURCE)], &[]);
    let krusty_errors = common::compiler_errors(&rendered);
    let reference_errors = common::compiler_errors(&reference_stderr);
    assert!(
        !reference_errors.is_empty(),
        "kotlinc reported no parseable error:\n{reference_stderr}"
    );
    assert_eq!(
        krusty_errors, reference_errors,
        "inspection diagnostics differ from kotlinc.\nkrusty:\n{rendered}\nkotlinc:\n{reference_stderr}"
    );
}
