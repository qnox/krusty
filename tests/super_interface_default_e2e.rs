//! `super.foo()` from a class implementing an interface dispatches to the interface's DEFAULT method
//! (JVM `invokespecial <iface>.foo`), even when the class has no super class. Runnable.
use super::common;
fn run(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, "Main")
}

#[test]
fn super_call_to_interface_default() {
    const SRC: &str = "interface I {\n\
        \x20 fun foo() = \"I.foo\"\n\
        \x20 fun bar(): String\n\
        }\n\
        class C : I {\n\
        \x20 override fun foo() = \"C.foo\"\n\
        \x20 override fun bar() = \"C.bar\"\n\
        \x20 fun viaSuper() = super.foo()\n\
        }\n\
        fun box(): String {\n\
        \x20 val c = C()\n\
        \x20 return if (c.foo() == \"C.foo\" && c.viaSuper() == \"I.foo\") \"OK\" else \"fail\"\n\
        }\n";
    assert_eq!(run(SRC).expect("super to interface default"), "OK");
}

#[test]
fn typed_super_selects_interface() {
    const SRC: &str = "interface T1 { fun foo() = \"O\" }\n\
        interface T2 { fun foo() = \"K\" }\n\
        class A : T1, T2 {\n\
        \x20 override fun foo() = super<T1>.foo() + super<T2>.foo()\n\
        }\n\
        fun box(): String = if (A().foo() == \"OK\") \"OK\" else \"fail: \" + A().foo()\n";
    assert_eq!(run(SRC).expect("typed super"), "OK");
}

#[test]
fn super_skips_abstract_class_override_for_interface_default() {
    const SRC: &str = "interface Test { fun test(): String = \"fail\" }\n\
        abstract class TestClass : Test { abstract override fun test(): String }\n\
        interface Test2 : Test { override fun test(): String = \"OK\" }\n\
        class TestClass2 : TestClass(), Test2 {\n\
        \x20 override fun test(): String = super.test()\n\
        }\n\
        fun box(): String = TestClass2().test()\n";
    assert_eq!(
        run(SRC).expect("abstract super/interface default diamond"),
        "OK"
    );
}
