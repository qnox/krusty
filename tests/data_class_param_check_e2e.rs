//! A `data class`'s primary-constructor parameters must all be `val`/`var` (kotlinc rejects a plain
//! parameter). Covers the class-arm data-class parameter check.

mod common;

fn diags(src: &str) -> Vec<String> {
    common::front_end_diagnostics(src, &[], None)
}

fn assert_rejected(src: &str) {
    let d = diags(src);
    assert!(
        d.iter()
            .any(|m| m.contains("data class") && m.contains("val") && m.contains("var")),
        "expected a data-class-parameter diagnostic, got: {d:?}\nsrc: {src}"
    );
}

fn assert_accepts(src: &str) {
    let d = diags(src);
    assert!(
        !d.iter()
            .any(|m| m.contains("data class") && m.contains("must be")),
        "unexpected data-class-parameter diagnostic: {d:?}\nsrc: {src}"
    );
}

#[test]
fn plain_param_rejected() {
    assert_rejected("data class D(x: Int)");
}

#[test]
fn mixed_val_and_plain_rejected() {
    assert_rejected("data class P(val a: Int, b: String)");
}

#[test]
fn all_val_ok() {
    assert_accepts("data class P(val x: Int, val y: Int)");
}

#[test]
fn all_var_ok() {
    assert_accepts("data class P(var x: Int, var y: String)");
}

#[test]
fn mixed_val_var_ok() {
    assert_accepts("data class P(val x: Int, var y: Int)");
}

#[test]
fn non_data_class_plain_param_ok() {
    // A regular class may have plain (non-property) constructor parameters.
    assert_accepts("class C(x: Int)");
}
