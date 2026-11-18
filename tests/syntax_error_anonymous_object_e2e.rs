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

/// An expect-class member is a header. A syntax error elsewhere in the file must not report
/// `allowed` as a function without a body, and the remaining diagnostics match kotlinc in order.
#[test]
fn an_expect_class_member_beside_a_syntax_error_matches_kotlinc() {
    const SOURCE: &str = "// LANGUAGE: +MultiPlatformProjects\nexpect class Expected {\n    fun allowed(): Int\n}\nfun g(): Int = object { fun f() = 1 }.f()\nfun broken(\n";
    let mut diagnostics = krusty::diag::DiagSink::new();
    let (_file, _symbols, _info) =
        krusty::frontend::analyze_source_standalone(SOURCE, &mut diagnostics);
    let rendered = diagnostics.render("Expect.kt", SOURCE);
    let reference_args = common::language_directives::kotlinc_args(SOURCE);
    let (_code, reference_stderr) =
        common::reference_compiler_run(&[("Expect.kt", SOURCE)], &reference_args);
    let krusty_errors = common::compiler_errors(&rendered);
    let reference_errors = common::compiler_errors(&reference_stderr);
    assert!(
        !reference_errors.is_empty(),
        "kotlinc reported no parseable error:\n{reference_stderr}"
    );
    assert!(
        krusty_errors
            .iter()
            .all(|error| !error.message.contains("allowed")),
        "an expect member was reported as missing a body:\n{rendered}"
    );
    assert_eq!(
        krusty_errors, reference_errors,
        "inspection diagnostics differ from kotlinc.\nkrusty:\n{rendered}\nkotlinc:\n{reference_stderr}"
    );
}
