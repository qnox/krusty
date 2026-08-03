//! Cannot construct an abstract class, sealed class, or interface directly (kotlinc rejects it; the
//! JVM would throw at `new`). Covers the `ctor_result` construction check.

use super::common;

fn diags(src: &str) -> Vec<String> {
    let stdlib = common::stdlib_jar();
    let jdk = common::jdk_modules();
    common::front_end_diagnostics(src, &[stdlib], Some(jdk.as_path()))
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
fn abstract_class_cannot_use_companion_factory_syntax() {
    assert_rejected(
        "abstract class Base {\n\
         \x20   companion object {\n\
         \x20       operator fun invoke(): Base = Implementation()\n\
         \x20   }\n\
         }\n\
         private class Implementation : Base()\n\
         fun f() { val value = Base() }",
    );
}

#[test]
fn instantiate_interface_with_members() {
    assert_rejected("interface Shape { fun area(): Double }\nfun f() { val s = Shape() }");
}

#[test]
fn reference_interface_constructor() {
    assert_rejected("interface Shape\nval make: () -> Shape = ::Shape");
}

#[test]
fn reference_abstract_constructor() {
    assert_rejected("abstract class Shape\nval make: () -> Shape = ::Shape");
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

#[test]
fn interface_name_can_call_companion_factory() {
    let src = "interface Contract {\n\
        \x20   val value: Int\n\
        \x20   companion object {\n\
        \x20       operator fun invoke(value: Int): Contract = Implementation(value)\n\
        \x20   }\n\
        }\n\
        private class Implementation(override val value: Int) : Contract\n\
        fun box(): String = if (Contract(31).value == 31) \"OK\" else \"fail\"\n";
    common::expect_box_ok_with_stdlib(src, "interface companion factory");
}

#[test]
fn companion_factory_uses_normal_argument_mapping() {
    let src = "interface Contract {\n\
        \x20   val value: Int\n\
        \x20   companion object {\n\
        \x20       operator fun invoke(offset: Int = 10, value: Int): Contract = Implementation(offset + value)\n\
        \x20   }\n\
        }\n\
        private class Implementation(override val value: Int) : Contract\n\
        fun box(): String {\n\
        \x20   val factory = Contract(value = 21)\n\
        \x20   return if (factory.value == 31) \"OK\" else \"fail\"\n\
        }\n";
    common::expect_box_ok_with_stdlib(src, "companion factory argument mapping");
}

#[test]
fn containing_class_can_call_private_companion_factory() {
    let src = "class Holder {\n\
        \x20   fun member(): Int = Holder.make()\n\
        \x20   companion object {\n\
        \x20       private fun make(): Int = 31\n\
        \x20   }\n\
        }\n\
        fun box(): String = if (Holder().member() == 31) \"OK\" else \"fail\"\n";
    common::expect_box_ok_with_stdlib(src, "containing class accesses private companion factory");
}

#[test]
fn sibling_companion_factory_uses_normal_argument_mapping() {
    let sources = [
        (
            "Main.kt",
            "package consumer\n\
             fun box(): String {\n\
             \x20   val factory = Contract(value = 21)\n\
             \x20   return if (factory.value == 31) \"OK\" else \"fail\"\n\
             }",
        ),
        (
            "Contract.kt",
            "package consumer\n\
             interface Contract {\n\
             \x20   val value: Int\n\
             \x20   companion object {\n\
             \x20       operator fun invoke(offset: Int = 10, value: Int): Contract = Implementation(offset + value)\n\
             \x20   }\n\
             }\n\
             private class Implementation(override val value: Int) : Contract",
        ),
    ];
    common::expect_box_ok_files_with_stdlib(&sources, "sibling companion factory argument mapping");
}
