//! Entry-point `main` functions are exempt from CROSS-FILE conflicting-overload diagnostics:
//! each file's facade class hosts its own `main`, so kotlinc accepts one `main` per file even with
//! identical signatures, while same-file duplicates and same-named non-`main` functions still
//! conflict. Verified against kotlinc 2.4.10.

use super::common;

fn diagnostics(sources: &[&str]) -> Vec<String> {
    let mut diags = common::front_end_diagnostics_files_with_stdlib(sources);
    diags.sort();
    diags
}

#[test]
fn cross_file_main_functions_do_not_conflict() {
    let diags = diagnostics(&[
        "package p\nfun main(rawArgs: Array<String>) {}\n",
        "package p\nfun main(rawArgs: Array<String>) {}\n",
    ]);
    assert_eq!(diags, Vec::<String>::new());
}

#[test]
fn cross_file_main_exemption_holds_for_any_signature() {
    let diags = diagnostics(&[
        "package p\nfun main(x: Int) {}\n",
        "package p\nfun main(x: Int) {}\n",
    ]);
    assert_eq!(diags, Vec::<String>::new());
}

#[test]
fn same_file_main_duplicates_still_conflict() {
    let diags = diagnostics(&[
        "package p\nfun main(rawArgs: Array<String>) {}\nfun main(rawArgs: Array<String>) {}\n",
    ]);
    assert_eq!(diags.len(), 2, "{diags:?}");
    assert!(
        diags
            .iter()
            .all(|d| d.starts_with("conflicting overloads:")),
        "{diags:?}"
    );
}

#[test]
fn cross_file_non_main_duplicates_still_conflict() {
    let diags = diagnostics(&[
        "package p\nfun helper(rawArgs: Array<String>) {}\n",
        "package p\nfun helper(rawArgs: Array<String>) {}\n",
    ]);
    assert_eq!(diags.len(), 2, "{diags:?}");
    assert!(
        diags
            .iter()
            .all(|d| d.starts_with("conflicting overloads:")),
        "{diags:?}"
    );
}
