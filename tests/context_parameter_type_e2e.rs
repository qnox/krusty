//! An anonymous object inside a function whose context parameter type is unresolved.
//! Publishing that receiver used to panic with "a context parameter's type is published: Error"
//! before the unresolved type could be reported.

use super::common;
use super::diagnostics_parity_support::complete_errors;

const SOURCE: &str = "\
context(_: Missing)
fun f() = object {}
";

#[test]
fn an_unresolved_context_parameter_matches_kotlinc() {
    let stdlib = common::stdlib_jar();
    let result =
        common::compiler_diagnostics(&[("Context.kt", SOURCE)], std::slice::from_ref(&stdlib));
    assert_ne!(
        result.reference_code, 0,
        "kotlinc accepted the context parameter"
    );
    assert_ne!(
        result.krusty_code, 0,
        "krusty accepted the context parameter"
    );
    let observed = complete_errors(&result.krusty_stderr, false);
    let expected = complete_errors(&result.reference_stderr, true);
    assert!(
        !expected.is_empty(),
        "kotlinc reported no error:\n{}",
        result.reference_stderr
    );
    assert_eq!(
        observed, expected,
        "inspection diagnostics differ from kotlinc.\nkrusty:\n{}\nkotlinc:\n{}",
        result.krusty_stderr, result.reference_stderr
    );
}
