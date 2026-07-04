//! Duplicate enum entry names (`enum class E { A, B, A }`) are illegal (conflicting declaration).
//! Covers the class-arm enum-entry duplicate scan.

use super::common;

fn diags(src: &str) -> Vec<String> {
    common::front_end_diagnostics(src, &[], None)
}

fn assert_rejected(src: &str) {
    let d = diags(src);
    assert!(
        d.iter()
            .any(|m| m.contains("enum entry") && m.contains("more than once")),
        "expected a duplicate-enum-entry diagnostic, got: {d:?}\nsrc: {src}"
    );
}

fn assert_accepts(src: &str) {
    let d = diags(src);
    assert!(
        !d.iter()
            .any(|m| m.contains("enum entry") && m.contains("more than once")),
        "unexpected duplicate-enum-entry diagnostic: {d:?}\nsrc: {src}"
    );
}

#[test]
fn two_entries_same_name() {
    assert_rejected("enum class E { A, B, A }");
}

#[test]
fn adjacent_duplicates() {
    assert_rejected("enum class Color { RED, RED }");
}

#[test]
fn distinct_entries_ok() {
    assert_accepts("enum class E { A, B, C }");
}

#[test]
fn single_entry_ok() {
    assert_accepts("enum class E { ONLY }");
}

#[test]
fn entries_with_args_ok() {
    assert_accepts("enum class P(val n: Int) { A(1), B(2), C(3) }");
}
