//! A `reified` type parameter is only allowed on an `inline` function (kotlinc rejects it otherwise).
//! Covers the check_fun reified/inline check.

use super::common;

fn diags(src: &str) -> Vec<String> {
    common::front_end_diagnostics(src, &[], None)
}

fn assert_rejected(src: &str) {
    let d = diags(src);
    assert!(
        d.iter()
            .any(|m| m.contains("'reified' type parameter is only allowed on an 'inline'")),
        "expected a reified diagnostic, got: {d:?}\nsrc: {src}"
    );
}

fn assert_accepts(src: &str) {
    let d = diags(src);
    assert!(
        !d.iter().any(|m| m.contains("'reified' type parameter")),
        "unexpected reified diagnostic: {d:?}\nsrc: {src}"
    );
}

#[test]
fn reified_on_non_inline_rejected() {
    assert_rejected("fun <reified T> f() {}");
}

#[test]
fn reified_second_param_on_non_inline_rejected() {
    assert_rejected("fun <A, reified B> f() {}");
}

#[test]
fn reified_on_inline_ok() {
    assert_accepts("inline fun <reified T> f() {}");
}

#[test]
fn plain_generic_non_inline_ok() {
    assert_accepts("fun <T> f(x: T): T = x");
}

#[test]
fn inline_without_reified_ok() {
    assert_accepts("inline fun f(block: () -> Unit) { block() }");
}
