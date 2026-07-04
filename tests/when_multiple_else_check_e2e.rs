//! A `when` may have at most one `else` branch (kotlinc rejects a second). Covers the `Expr::When`
//! else-count check.

use super::common;

fn diags(src: &str) -> Vec<String> {
    common::front_end_diagnostics(src, &[], None)
}

fn assert_rejected(src: &str) {
    let d = diags(src);
    assert!(
        d.iter().any(|m| m.contains("at most one 'else' branch")),
        "expected a multiple-else diagnostic, got: {d:?}\nsrc: {src}"
    );
}

fn assert_accepts(src: &str) {
    let d = diags(src);
    assert!(
        !d.iter().any(|m| m.contains("at most one 'else' branch")),
        "unexpected multiple-else diagnostic: {d:?}\nsrc: {src}"
    );
}

#[test]
fn two_else_with_subject() {
    assert_rejected("fun f(x: Int) = when (x) { 1 -> \"a\"; else -> \"b\"; else -> \"c\" }");
}

#[test]
fn two_else_subjectless() {
    assert_rejected("fun f(x: Int) = when { x > 0 -> \"a\"; else -> \"b\"; else -> \"c\" }");
}

#[test]
fn one_else_ok() {
    assert_accepts("fun f(x: Int) = when (x) { 1 -> \"a\"; else -> \"b\" }");
}

#[test]
fn no_else_ok() {
    assert_accepts("fun f(x: Int) { when (x) { 1 -> {}; 2 -> {} } }");
}

#[test]
fn many_branches_one_else_ok() {
    assert_accepts(
        "fun f(x: Int) = when (x) { 1 -> \"a\"; 2 -> \"b\"; 3 -> \"c\"; else -> \"d\" }",
    );
}
