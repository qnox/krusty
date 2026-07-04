//! Cannot construct an abstract class, sealed class, or interface directly (kotlinc rejects it; the
//! JVM would throw at `new`). Covers the `ctor_result` construction check.

use super::common;

fn diags(src: &str) -> Vec<String> {
    let Some(stdlib) = common::stdlib_jar() else {
        return vec!["<skip: no stdlib>".into()];
    };
    let jdk = common::jdk_modules();
    common::front_end_diagnostics(src, &[stdlib], jdk.as_deref())
}

fn assert_rejected(src: &str) {
    let d = diags(src);
    if d.iter().any(|m| m == "<skip: no stdlib>") {
        return;
    }
    assert!(
        d.iter().any(|m| m.contains("cannot create an instance")),
        "expected an abstract/interface-instantiation diagnostic, got: {d:?}\nsrc: {src}"
    );
}

fn assert_accepts(src: &str) {
    let d = diags(src);
    if d.iter().any(|m| m == "<skip: no stdlib>") {
        return;
    }
    assert!(
        !d.iter().any(|m| m.contains("cannot create an instance")),
        "unexpected instantiation diagnostic on valid code: {d:?}\nsrc: {src}"
    );
}

// ---- REJECTED --------------------------------------------------------------

#[test]
fn instantiate_interface() {
    assert_rejected("interface I\nfun f() { val x = I() }");
}

#[test]
fn instantiate_abstract_class() {
    assert_rejected("abstract class A\nfun f() { val x = A() }");
}

#[test]
fn instantiate_sealed_class() {
    assert_rejected("sealed class S\nfun f() { val x = S() }");
}

#[test]
fn instantiate_abstract_with_ctor_params() {
    assert_rejected("abstract class A(val n: Int)\nfun f() { val x = A(1) }");
}

#[test]
fn instantiate_interface_with_members() {
    assert_rejected("interface Shape { fun area(): Double }\nfun f() { val s = Shape() }");
}

// ---- ACCEPTED --------------------------------------------------------------

#[test]
fn concrete_class() {
    assert_accepts("class C\nfun f() { val x = C() }");
}

#[test]
fn subclass_of_open() {
    assert_accepts("open class O\nclass D : O()\nfun f() { val x = D() }");
}

#[test]
fn subclass_of_abstract() {
    assert_accepts("abstract class A\nclass B : A()\nfun f() { val x = B() }");
}

#[test]
fn interface_implementation() {
    assert_accepts("interface I\nclass Impl : I\nfun f() { val x: I = Impl() }");
}

#[test]
fn object_expression_implementing_interface() {
    assert_accepts(
        "interface I { fun g(): Int }\nfun f() { val i = object : I { override fun g() = 1 } }",
    );
}

#[test]
fn subclass_super_delegation_not_flagged() {
    assert_accepts(
        "abstract class A(val n: Int)\nclass B(n: Int) : A(n)\nfun f() { val x = B(5) }",
    );
}

#[test]
fn data_class_construction() {
    assert_accepts("data class P(val x: Int, val y: Int)\nfun f() { val p = P(1, 2) }");
}
