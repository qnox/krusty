//! An `abstract` member is only allowed in an abstract class or an interface (kotlinc rejects
//! `class C { abstract fun f() }`). Covers the class-arm abstract-member check.

mod common;

fn diags(src: &str) -> Vec<String> {
    common::front_end_diagnostics(src, &[], None)
}

fn assert_rejected(src: &str) {
    let d = diags(src);
    assert!(
        d.iter()
            .any(|m| m.contains("abstract member") && m.contains("non-abstract class")),
        "expected an abstract-member diagnostic, got: {d:?}\nsrc: {src}"
    );
}

fn assert_accepts(src: &str) {
    let d = diags(src);
    assert!(
        !d.iter()
            .any(|m| m.contains("abstract member") && m.contains("non-abstract class")),
        "unexpected abstract-member diagnostic: {d:?}\nsrc: {src}"
    );
}

#[test]
fn abstract_fun_in_final_class() {
    assert_rejected("class C { abstract fun f() }");
}

#[test]
fn abstract_fun_in_plain_class_with_members() {
    assert_rejected("class Shape { fun area(): Double = 0.0; abstract fun draw() }");
}

#[test]
fn abstract_fun_in_abstract_class_ok() {
    assert_accepts("abstract class C { abstract fun f() }");
}

#[test]
fn abstract_fun_in_interface_ok() {
    assert_accepts("interface I { fun f(): Int }");
}

#[test]
fn abstract_fun_in_sealed_class_ok() {
    assert_accepts("sealed class S { abstract fun f(): Int }");
}

#[test]
fn concrete_members_in_final_class_ok() {
    assert_accepts("class C { fun f(): Int = 1; fun g() {} }");
}

#[test]
fn open_member_in_final_class_ok() {
    assert_accepts("open class C { open fun f(): Int = 1 }");
}

#[test]
fn abstract_member_in_enum_ok() {
    // An enum class may declare an abstract member that each entry overrides.
    assert_accepts("enum class Op { ADD { override fun apply(a: Int, b: Int) = a + b }; abstract fun apply(a: Int, b: Int): Int }");
}
