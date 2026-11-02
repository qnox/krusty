//! An ambiguous call in an expression body whose result is inferred is selected twice: once while
//! the declaration's signature is solved and once by the body check. Both name every candidate by
//! its resolved signature, so the call reports kotlinc's one ambiguity, whichever candidate's
//! result was still being inferred when the call failed.

use super::common;
use super::diagnostics_parity_support::complete_errors;

fn assert_complete_error_parity(source: &str) {
    let stdlib = common::stdlib_jar();
    let result = common::compiler_diagnostics(&[("Ambiguity.kt", source)], &[stdlib]);
    let mut krusty = complete_errors(&result.krusty_stderr, false);
    krusty.extend(complete_errors(&result.krusty_stdout, false));
    let reference = complete_errors(&result.reference_stderr, true);
    assert_ne!(result.reference_code, 0, "kotlinc must reject {source:?}");
    assert_ne!(result.krusty_code, 0, "krusty must reject {source:?}");
    assert_eq!(krusty, reference, "diagnostics differ for {source:?}");
}

#[test]
fn a_top_level_ambiguity_names_inferred_results_once() {
    assert_complete_error_parity(
        "fun f(a: Any, b: String) = 1\nfun f(a: String, b: Any) = 2\nfun x() = f(\"\", \"\")\n",
    );
}

#[test]
fn an_extension_ambiguity_names_inferred_results_once() {
    assert_complete_error_parity(
        "fun Int.f(a: Any, b: String) = 1\nfun Int.f(a: String, b: Any) = 2\nfun x() = 1.f(\"\", \"\")\n",
    );
}

#[test]
fn a_member_ambiguity_names_inferred_results_once() {
    assert_complete_error_parity(
        "class C { fun g(a: Any, b: String) = 1; fun g(a: String, b: Any) = \"\" }\nfun y(c: C) = c.g(\"\", \"\")\n",
    );
}

#[test]
fn a_candidate_declared_after_the_call_is_named_by_its_inferred_result() {
    assert_complete_error_parity(
        "fun x() = f(\"\", \"\")\nfun f(a: Any, b: String) = listOf(a)\nfun f(a: String, b: Any) = b\n",
    );
}

#[test]
fn a_generic_ambiguity_names_inferred_results_once() {
    assert_complete_error_parity(
        "fun <T> f(a: T, b: String) = listOf(a)\nfun <T> f(a: String, b: T) = b\nfun x() = f(\"\", \"\")\n",
    );
}
