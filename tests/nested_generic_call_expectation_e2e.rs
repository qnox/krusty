//! Nested generic calls under an expected type must be checked in time linear in the nesting.
//!
//! After a callable is selected, every argument is re-entered under the selected parameter type
//! so the expectation can reach a nested generic call's result variables (`"OK" to emptySet()`
//! under `Pair<Any, Set<Any>>`). Re-entering an argument whose inferred type already EQUALS that
//! expectation is idempotent, yet each level of `mapOf("k" to mapOf("k" to …))` used to redo the
//! whole subtree — `to` and `mapOf` each re-entering theirs — so a six-level literal in a real
//! test file took minutes and stalled the editor's analysis worker.

use super::common;

fn nested(depth: usize) -> String {
    let mut inner = String::from("1");
    for level in 0..depth {
        inner = format!("mapOf(\"k{level}\" to {inner})");
    }
    format!("package sample\nval root: Map<String, Any?> = {inner}\nfun n(): Int = root.size\n")
}

#[test]
fn a_deeply_nested_map_literal_checks_in_linear_time() {
    let stdlib = common::stdlib_jar();
    let jdk = common::jdk_modules();
    let started = std::time::Instant::now();
    let d = common::front_end_diagnostics(
        &nested(9),
        std::slice::from_ref(&stdlib),
        Some(jdk.as_path()),
    );
    let elapsed = started.elapsed();
    assert_eq!(d, Vec::<String>::new());
    assert!(
        elapsed < std::time::Duration::from_secs(30),
        "nine nested generic calls took {elapsed:?}; the argument re-check is exponential again"
    );
}

#[test]
fn an_expectation_still_reaches_a_nested_generic_call_result() {
    // The re-entry this test's sibling bounds must still happen when it matters: an
    // unconstrained inner call is typed by the outer parameter.
    let stdlib = common::stdlib_jar();
    let jdk = common::jdk_modules();
    let d = common::front_end_diagnostics(
        "package sample\nfun take(p: Pair<Any, Set<Any>>): Int = p.second.size\nfun n(): Int = take(\"OK\" to emptySet())\n",
        std::slice::from_ref(&stdlib),
        Some(jdk.as_path()),
    );
    assert_eq!(d, Vec::<String>::new());
}
