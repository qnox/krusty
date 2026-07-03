//! `abstract` modifier consistency: an abstract member has no body and cannot also be `private` or
//! `final` (kotlinc rejects each). Covers the class-arm consistency loop.

mod common;

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
fn abstract_with_block_body() {
    assert_has(
        "abstract class C { abstract fun f() { } }",
        "cannot have a body",
    );
}

#[test]
fn abstract_with_expression_body() {
    assert_has(
        "abstract class C { abstract fun f(): Int = 1 }",
        "cannot have a body",
    );
}

#[test]
fn abstract_and_final() {
    assert_has(
        "abstract class C { abstract final fun f() }",
        "'abstract' and 'final'",
    );
}

#[test]
fn abstract_no_body_ok() {
    assert_lacks(
        "abstract class C { abstract fun f() }",
        "cannot have a body",
    );
}

#[test]
fn concrete_with_body_ok() {
    assert_lacks("class C { fun f(): Int = 1 }", "cannot have a body");
}

#[test]
fn abstract_member_not_private_or_final_ok() {
    let d = diags("abstract class C { abstract fun f(): Int }");
    assert!(
        !d.iter().any(|m| m.contains("'abstract' and")),
        "false positive: {d:?}"
    );
}
