//! kotlinc has no "unresolved function" diagnostic. A callee that resolves to nothing at all is
//! UNRESOLVED_REFERENCE — the same diagnostic a bare unresolved name gets — rendered as
//! `Unresolved reference 'x'.` by the IntelliJ Kotlin language server and reported lowercase-first
//! by the compiler. These pin the exact text, including the trailing period.

use super::common;

#[test]
fn a_call_to_an_undeclared_function_is_an_unresolved_reference() {
    const SOURCE: &str = r#"
        fun use(): Int {
            noSuchFunction()
            return 0
        }
    "#;

    let Some(diagnostics) = common::checker_diags_with_stdlib(SOURCE) else {
        return;
    };
    assert!(
        diagnostics
            .iter()
            .any(|diagnostic| diagnostic == "unresolved reference 'noSuchFunction'."),
        "expected kotlinc's UNRESOLVED_REFERENCE wording, got: {diagnostics:?}"
    );
}

#[test]
fn a_call_to_an_undeclared_classifier_is_an_unresolved_reference() {
    const SOURCE: &str = r#"
        fun use(): Any = NoSuchClass(1)
    "#;

    let Some(diagnostics) = common::checker_diags_with_stdlib(SOURCE) else {
        return;
    };
    assert!(
        diagnostics
            .iter()
            .any(|diagnostic| diagnostic == "unresolved reference 'NoSuchClass'."),
        "a constructor-shaped call resolves no differently, got: {diagnostics:?}"
    );
}
