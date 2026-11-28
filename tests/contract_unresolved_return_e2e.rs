//! A contract is published with its callable. An unresolved return type used to drop that
//! callable, and signature finalization panicked with "a contract requires a published callable
//! signature" before the unresolved type and the contract opt-in could be reported. The failed
//! signature stays failed: a non-error return is not checked against a stand-in result.

use super::common;
use super::diagnostics_parity_support::complete_errors;

const SOURCE: &str = "\
import kotlin.contracts.*

fun f(v: Any?): Missing {
    contract { returns() implies (v != null) }
    return Missing()
}
";

#[test]
fn a_contract_on_an_unresolved_return_type_matches_kotlinc() {
    let stdlib = common::stdlib_jar();
    let result =
        common::compiler_diagnostics(&[("Contract.kt", SOURCE)], std::slice::from_ref(&stdlib));
    assert_ne!(result.reference_code, 0, "kotlinc accepted the contract");
    assert_ne!(result.krusty_code, 0, "krusty accepted the contract");
    let observed = complete_errors(&result.krusty_stderr, true);
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

const RETURN_VALUE: &str = "\
import kotlin.contracts.*

fun f(v: Any?): Missing {
    contract { returns() implies (v != null) }
    return 1
}
";

#[test]
fn a_non_error_return_is_not_checked_against_a_fabricated_result() {
    let stdlib = common::stdlib_jar();
    let result = common::compiler_diagnostics(
        &[("ContractReturn.kt", RETURN_VALUE)],
        std::slice::from_ref(&stdlib),
    );
    assert_ne!(result.reference_code, 0, "kotlinc accepted the contract");
    assert_ne!(result.krusty_code, 0, "krusty accepted the contract");
    let observed = complete_errors(&result.krusty_stderr, true);
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
