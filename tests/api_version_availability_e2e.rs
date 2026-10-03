//! `-api-version` hides a dependency callable whose `@SinceKotlin` is newer than that level.
//! Kotlinc leaves the name unresolved rather than reporting a separate availability diagnostic.

use super::common;

const SORTED: &str = "fun box(): Boolean = intArrayOf(1, 2).isSorted()\n";

fn version_args(api: &str) -> Vec<String> {
    vec![
        "-language-version".to_string(),
        "2.4".to_string(),
        "-api-version".to_string(),
        api.to_string(),
    ]
}

#[test]
fn an_older_api_hides_a_newer_since_kotlin_extension() {
    common::assert_errors_match_kotlinc(&[("main.kt", SORTED)], &version_args("2.0"));
}

#[test]
fn the_current_api_keeps_a_since_kotlin_extension() {
    let sources = [("main.kt", SORTED)];
    let args = version_args("2.4");
    let expected = common::reference_error_ledger(&sources, &args);
    assert_eq!(
        common::krusty_error_ledger_with_args(&sources, &args),
        expected
    );
    assert!(expected.is_empty(), "kotlinc accepts isSorted at API 2.4");
}
