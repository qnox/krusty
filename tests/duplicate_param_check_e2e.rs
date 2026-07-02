//! Duplicate function parameter names are illegal (kotlinc: conflicting declaration). Covers the
//! `check_fun` parameter-name check.

mod common;

fn diags(src: &str) -> Vec<String> {
    common::front_end_diagnostics(src, &[], None)
}

fn assert_rejected(src: &str) {
    let d = diags(src);
    assert!(
        d.iter().any(|m| m.contains("declared more than once")),
        "expected a duplicate-parameter diagnostic, got: {d:?}\nsrc: {src}"
    );
}

fn assert_accepts(src: &str) {
    let d = diags(src);
    assert!(
        !d.iter().any(|m| m.contains("declared more than once")),
        "unexpected duplicate-parameter diagnostic: {d:?}\nsrc: {src}"
    );
}

#[test]
fn two_params_same_name() {
    assert_rejected("fun f(x: Int, x: Int): Int { return x }");
}

#[test]
fn three_params_one_dup() {
    assert_rejected("fun f(a: Int, b: Int, a: String) {}");
}

#[test]
fn dup_among_many() {
    assert_rejected("fun f(p: Int, q: Int, r: Int, q: Int) {}");
}

#[test]
fn distinct_params_ok() {
    assert_accepts("fun f(x: Int, y: Int, z: Int): Int { return x + y + z }");
}

#[test]
fn single_param_ok() {
    assert_accepts("fun f(x: Int): Int { return x }");
}

#[test]
fn no_params_ok() {
    assert_accepts("fun f(): Int { return 0 }");
}

#[test]
fn same_name_different_functions_ok() {
    // The same name in two SEPARATE functions is fine — the check is per-function.
    assert_accepts("fun f(x: Int): Int { return x }\nfun g(x: Int): Int { return x }");
}
