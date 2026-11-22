//! Inapplicable `setValue` candidates are retained for the convention diagnostic. When the
//! property type is already unresolved, that diagnostic used to be absent and the checker
//! panicked with "retained delegate candidates must produce a diagnostic".

use super::common;
use super::diagnostics_parity_support::complete_errors;

const SOURCE: &str = "\
class Delegate {
    operator fun getValue(thisRef: String, property: Any?): Int = 1
    operator fun setValue(thisRef: String, property: Any?, value: Int) {}
}

class Host {
    var p: Missing by Delegate()
}
";

#[test]
fn an_unresolved_delegated_property_matches_kotlinc() {
    let stdlib = common::stdlib_jar();
    let result =
        common::compiler_diagnostics(&[("Delegate.kt", SOURCE)], std::slice::from_ref(&stdlib));
    assert_ne!(result.reference_code, 0, "kotlinc accepted the delegate");
    assert_ne!(result.krusty_code, 0, "krusty accepted the delegate");
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

const GENERIC: &str = "\
class Delegate {
    operator fun getValue(thisRef: String, property: Any?): Int = 1
    operator fun setValue(thisRef: String, property: Any?, value: Int) {}
}

class Host {
    var p: Missing<String> by Delegate()
}
";

#[test]
fn an_unresolved_generic_delegated_property_matches_kotlinc() {
    let stdlib = common::stdlib_jar();
    let result = common::compiler_diagnostics(
        &[("DelegateGeneric.kt", GENERIC)],
        std::slice::from_ref(&stdlib),
    );
    assert_ne!(result.reference_code, 0, "kotlinc accepted the delegate");
    assert_ne!(result.krusty_code, 0, "krusty accepted the delegate");
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

const AMBIGUOUS_GET: &str = "\
class Delegate {
    operator fun getValue(thisRef: String, property: Any?): Int = 1
    operator fun getValue(thisRef: Int, property: Any?): String = \"\"
    operator fun setValue(thisRef: String, property: Any?, value: Int) {}
}

class Host {
    var p by Delegate()
}
";

#[test]
fn an_ambiguous_getvalue_on_an_unannotated_property_matches_kotlinc() {
    let stdlib = common::stdlib_jar();
    let result = common::compiler_diagnostics(
        &[("AmbiguousGet.kt", AMBIGUOUS_GET)],
        std::slice::from_ref(&stdlib),
    );
    assert_ne!(result.reference_code, 0, "kotlinc accepted the delegate");
    assert_ne!(result.krusty_code, 0, "krusty accepted the delegate");
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

const RESOLVED_OUTER: &str = "\
class Delegate {
    operator fun getValue(thisRef: String, property: Any?): Int = 1
    operator fun setValue(thisRef: String, property: Any?, value: Int) {}
}

class Host {
    var array: kotlin.Array<Missing> by Delegate()
    var list: List<Missing> by Delegate()
}
";

const UNANNOTATED: &str = "\
class Delegate {
    operator fun getValue(thisRef: String, property: Any?): Int = 1
    operator fun setValue(thisRef: String, property: Any?, value: Int) {}
}

class Host {
    var p by Delegate()
}
";

#[test]
fn an_unannotated_delegated_property_matches_kotlinc() {
    let stdlib = common::stdlib_jar();
    let result = common::compiler_diagnostics(
        &[("Unannotated.kt", UNANNOTATED)],
        std::slice::from_ref(&stdlib),
    );
    assert_ne!(result.reference_code, 0, "kotlinc accepted the delegate");
    assert_ne!(result.krusty_code, 0, "krusty accepted the delegate");
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

const MISSING_GET: &str = "\
class Delegate {
    operator fun setValue(thisRef: String, property: Any?, value: Int) {}
}

class Host {
    var p by Delegate()
}
";

#[test]
fn a_missing_getvalue_on_an_unannotated_property_matches_kotlinc() {
    let stdlib = common::stdlib_jar();
    let result = common::compiler_diagnostics(
        &[("MissingGet.kt", MISSING_GET)],
        std::slice::from_ref(&stdlib),
    );
    assert_ne!(result.reference_code, 0, "kotlinc accepted the delegate");
    assert_ne!(result.krusty_code, 0, "krusty accepted the delegate");
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

#[test]
fn a_resolved_generic_outer_type_keeps_its_unresolved_argument() {
    let stdlib = common::stdlib_jar();
    let result = common::compiler_diagnostics(
        &[("ResolvedOuter.kt", RESOLVED_OUTER)],
        std::slice::from_ref(&stdlib),
    );
    assert_ne!(result.reference_code, 0, "kotlinc accepted the delegate");
    assert_ne!(result.krusty_code, 0, "krusty accepted the delegate");
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
