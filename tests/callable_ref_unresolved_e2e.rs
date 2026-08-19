//! Callable references that cannot be resolved must produce kotlinc's `unresolved reference`
//! diagnostics — never the generic "callable references are not supported" cascade.
//!
//! kotlinc 2.4.10 reference behavior (pinned per test):
//! - `u::method` where `u`'s DECLARED type is unresolved: the receiver expression is valid, so the
//!   member name itself is reported (`unresolved reference 'method'`).
//! - `Unresolved::method` / `missingFn()::method`: the receiver expression already carries the
//!   error; no second diagnostic on the member name.
//! - `::missing`: the name is reported unresolved.
//! - `"abc"::nosuch`: a valid receiver type with no such member reports the name unresolved.

use super::common;

fn diagnostics(src: &str) -> Vec<String> {
    let mut diags = common::front_end_diagnostics_files_with_stdlib(&[src]);
    diags.sort();
    diags
}

#[test]
fn bound_ref_on_value_typed_by_unresolved_classifier_reports_the_member_name() {
    let diags = diagnostics(
        "fun f(u: Unresolved) {\n\
         \u{20}   val r = u::method\n\
         }\n",
    );
    assert_eq!(
        diags,
        vec![
            "unresolved reference 'Unresolved'.".to_string(),
            "unresolved reference 'method'.".to_string(),
        ],
    );
}

#[test]
fn unbound_ref_on_unresolved_classifier_reports_only_the_classifier() {
    let diags = diagnostics(
        "fun f() {\n\
                             \u{20}   val r = Unresolved::method\n\
                             }\n",
    );
    assert_eq!(
        diags,
        vec!["unresolved reference 'Unresolved'.".to_string()]
    );
}

#[test]
fn bound_ref_on_failed_receiver_call_adds_no_member_cascade() {
    let diags = diagnostics(
        "fun f() {\n\
                             \u{20}   val r = missingFn()::method\n\
                             }\n",
    );
    assert_eq!(diags, vec!["unresolved reference 'missingFn'.".to_string()]);
}

#[test]
fn receiver_less_ref_to_unknown_name_is_unresolved() {
    let diags = diagnostics(
        "fun f() {\n\
                             \u{20}   val r = ::missing\n\
                             }\n",
    );
    assert_eq!(diags, vec!["unresolved reference 'missing'.".to_string()]);
}

#[test]
fn valid_receiver_with_no_such_member_reports_the_name() {
    let diags = diagnostics(
        "fun f() {\n\
                             \u{20}   val r = \"abc\"::nosuch\n\
                             }\n",
    );
    assert_eq!(diags, vec!["unresolved reference 'nosuch'.".to_string()]);
}
