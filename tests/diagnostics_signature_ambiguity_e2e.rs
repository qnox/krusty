//! An ambiguous call in an expression body whose result is inferred is selected twice: once while
//! the declaration's signature is solved and once by the body check. Both name every candidate by
//! its resolved signature, so the call reports kotlinc's one ambiguity, whichever candidate's
//! result was still being inferred when the call failed.

use super::common;
use super::diagnostics_parity_support::complete_errors;

/// Each fixture owns exactly one ambiguity. Both compilers must reject it with the ordinary
/// rejection exit code and exactly that one error, krusty must print no error on stdout, and the
/// complete errors (file, line, column, message and candidate lines, in order) must be equal.
fn assert_one_identical_ambiguity(source: &str) {
    let stdlib = common::stdlib_jar();
    let result = common::compiler_diagnostics(&[("Ambiguity.kt", source)], &[stdlib]);
    let krusty = complete_errors(&result.krusty_stderr, false);
    let reference = complete_errors(&result.reference_stderr, true);
    assert_eq!(
        result.reference_code, 1,
        "kotlinc exited {} rather than rejecting {source:?}: {}",
        result.reference_code, result.reference_stderr
    );
    assert_eq!(
        result.krusty_code, 1,
        "krusty exited {} rather than rejecting {source:?}: {}{}",
        result.krusty_code, result.krusty_stdout, result.krusty_stderr
    );
    assert_eq!(
        complete_errors(&result.krusty_stdout, false),
        [],
        "krusty printed errors on stdout for {source:?}"
    );
    assert_eq!(
        reference.len(),
        1,
        "kotlinc must report exactly one error for {source:?}: {reference:#?}"
    );
    assert_eq!(
        krusty.len(),
        1,
        "krusty must report exactly one error for {source:?}: {krusty:#?}"
    );
    assert_eq!(krusty, reference, "diagnostics differ for {source:?}");
}

#[test]
fn a_top_level_ambiguity_names_inferred_results_once() {
    assert_one_identical_ambiguity(
        "fun f(a: Any, b: String) = 1\nfun f(a: String, b: Any) = 2\nfun x() = f(\"\", \"\")\n",
    );
}

#[test]
fn an_extension_ambiguity_names_inferred_results_once() {
    assert_one_identical_ambiguity(
        "fun Int.f(a: Any, b: String) = 1\nfun Int.f(a: String, b: Any) = 2\nfun x() = 1.f(\"\", \"\")\n",
    );
}

#[test]
fn a_member_ambiguity_names_inferred_results_once() {
    assert_one_identical_ambiguity(
        "class C { fun g(a: Any, b: String) = 1; fun g(a: String, b: Any) = \"\" }\nfun y(c: C) = c.g(\"\", \"\")\n",
    );
}

#[test]
fn a_candidate_declared_after_the_call_is_named_by_its_inferred_result() {
    assert_one_identical_ambiguity(
        "fun x() = f(\"\", \"\")\nfun f(a: Any, b: String) = listOf(a)\nfun f(a: String, b: Any) = b\n",
    );
}

#[test]
fn a_generic_ambiguity_names_inferred_results_once() {
    assert_one_identical_ambiguity(
        "fun <T> f(a: T, b: String) = listOf(a)\nfun <T> f(a: String, b: T) = b\nfun x() = f(\"\", \"\")\n",
    );
}
