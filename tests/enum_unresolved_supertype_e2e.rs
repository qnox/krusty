//! An enum whose supertype does not resolve publishes no classifier header, so its entries have no
//! semantic owner to select a constructor of. The unresolved reference is the whole report; the
//! checker used to die with a panic (exit 101 in the language server) on the entry list.

use super::common;

#[test]
fn an_enum_with_an_unresolved_supertype_reports_only_the_unresolved_reference() {
    let diagnostics = common::front_end_diagnostics(
        "enum class Choice : Missing { A, B }\n",
        &[common::stdlib_jar()],
        Some(common::jdk_modules().as_path()),
    );
    assert_eq!(
        diagnostics,
        vec!["unresolved reference 'Missing'.".to_string()]
    );
}
