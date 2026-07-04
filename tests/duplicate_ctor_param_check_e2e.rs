//! Duplicate primary-constructor parameter names are illegal (kotlinc: conflicting declaration).
//! Covers the class-check `cl.props` duplicate scan.

use super::common;

fn diags(src: &str) -> Vec<String> {
    common::front_end_diagnostics(src, &[], None)
}

fn assert_rejected(src: &str) {
    let d = diags(src);
    assert!(
        d.iter()
            .any(|m| m.contains("constructor parameter") && m.contains("more than once")),
        "expected a duplicate-ctor-param diagnostic, got: {d:?}\nsrc: {src}"
    );
}

fn assert_accepts(src: &str) {
    let d = diags(src);
    assert!(
        !d.iter()
            .any(|m| m.contains("constructor parameter") && m.contains("more than once")),
        "unexpected duplicate-ctor-param diagnostic: {d:?}\nsrc: {src}"
    );
}

#[test]
fn two_val_params_same_name() {
    assert_rejected("class C(val x: Int, val x: Int)");
}

#[test]
fn val_and_plain_same_name() {
    assert_rejected("class C(val x: Int, x: Int)");
}

#[test]
fn dup_among_three() {
    assert_rejected("class C(val a: Int, val b: Int, val a: String)");
}

#[test]
fn distinct_params_ok() {
    assert_accepts("class C(val x: Int, val y: Int)");
}

#[test]
fn data_class_ok() {
    assert_accepts("data class P(val x: Int, val y: Int, val z: Int)");
}

#[test]
fn single_param_ok() {
    assert_accepts("class C(val x: Int)");
}

#[test]
fn no_primary_ctor_ok() {
    assert_accepts("class C");
}
