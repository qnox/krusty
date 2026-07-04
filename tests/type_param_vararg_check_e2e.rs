//! Duplicate type-parameter names (`fun <T, T>`, `class C<T, T>`) and multiple `vararg` parameters
//! are illegal (kotlinc: conflicting declaration / multiple vararg parameters not allowed). Covers
//! the check_fun and class-arm scans.

use super::common;

fn diags(src: &str) -> Vec<String> {
    common::front_end_diagnostics(src, &[], None)
}

fn assert_has(src: &str, needle: &str) {
    let d = diags(src);
    assert!(
        d.iter().any(|m| m.contains(needle)),
        "expected '{needle}', got: {d:?}\nsrc: {src}"
    );
}

fn assert_lacks(src: &str, needle: &str) {
    let d = diags(src);
    assert!(
        !d.iter().any(|m| m.contains(needle)),
        "unexpected '{needle}': {d:?}\nsrc: {src}"
    );
}

#[test]
fn dup_fn_type_param() {
    assert_has(
        "fun <T, T> f(x: T): T = x",
        "type parameter 'T' is declared more than once",
    );
}

#[test]
fn dup_class_type_param() {
    assert_has(
        "class C<T, T>",
        "type parameter 'T' is declared more than once",
    );
}

#[test]
fn dup_among_three_type_params() {
    assert_has(
        "fun <A, B, A> f() {}",
        "type parameter 'A' is declared more than once",
    );
}

#[test]
fn multiple_vararg_params() {
    assert_has(
        "fun f(vararg a: Int, vararg b: Int) {}",
        "multiple vararg parameters are not allowed",
    );
}

#[test]
fn distinct_fn_type_params_ok() {
    assert_lacks("fun <T, U> f(x: T, y: U): T = x", "more than once");
}

#[test]
fn distinct_class_type_params_ok() {
    assert_lacks("class Box<A, B>(val a: A, val b: B)", "more than once");
}

#[test]
fn single_vararg_ok() {
    assert_lacks("fun f(vararg a: Int, y: Int) {}", "vararg");
}

#[test]
fn no_type_params_ok() {
    assert_lacks("fun f(x: Int): Int = x", "more than once");
}
