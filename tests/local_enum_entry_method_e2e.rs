//! A local enum is illegal. Its entry method is still a streamed callable, and checking it used
//! to panic with "a streamed enum-entry method must have a finalized callable header" before the
//! modifier diagnostic could be reported.

use super::common;

const SOURCE: &str = "fun outer() {\n    enum class E {\n        A {\n            fun f() = 1\n        }\n    }\n}\n";

#[test]
fn a_local_enum_entry_method_matches_kotlinc() {
    let mut diagnostics = krusty::diag::DiagSink::new();
    let (_file, _symbols, _info) =
        krusty::frontend::analyze_source_standalone(SOURCE, &mut diagnostics);
    let rendered = diagnostics.render("LocalEnum.kt", SOURCE);
    let (_code, reference_stderr) =
        common::reference_compiler_run(&[("LocalEnum.kt", SOURCE)], &[]);
    let krusty_errors = common::compiler_errors(&rendered);
    let reference_errors = common::compiler_errors(&reference_stderr);
    assert!(
        !reference_errors.is_empty(),
        "kotlinc reported no error:\n{reference_stderr}"
    );
    assert_eq!(
        krusty_errors, reference_errors,
        "inspection diagnostics differ from kotlinc.\nkrusty:\n{rendered}\nkotlinc:\n{reference_stderr}"
    );
}
