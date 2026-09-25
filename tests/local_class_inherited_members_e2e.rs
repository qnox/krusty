//! A member call on a local class or anonymous object sees every member its supertypes inherit,
//! not only the members its direct supertypes declare, and an anonymous object type that escapes
//! its declaring body keeps the provider's (substituted) member scope.
use super::common;
fn run(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, "Main")
}

#[test]
fn local_class_calls_member_declared_two_supertypes_up() {
    const SRC: &str = "open class A { fun ok() = \"OK\" }\n\
        open class B : A()\n\
        fun box(): String { class L : B(); return L().ok() }\n";
    assert_eq!(run(SRC).expect("local class inherited member"), "OK");
}

#[test]
fn anonymous_object_calls_member_of_super_interface() {
    const SRC: &str = "interface I { fun ok() = \"OK\" }\n\
        interface II : I\n\
        fun box() = (object : II {}).ok()\n";
    assert_eq!(run(SRC).expect("anonymous object inherited member"), "OK");
}

#[test]
fn local_class_extends_local_class_extending_generic_class() {
    const SRC: &str = "open class A<T>(val t: T) { fun get(): T = t }\n\
        fun box(): String {\n\
        \x20 open class L1 : A<String>(\"OK\")\n\
        \x20 class L2 : L1()\n\
        \x20 return L2().get()\n\
        }\n";
    assert_eq!(run(SRC).expect("local chain generic member"), "OK");
}

#[test]
fn escaping_anonymous_object_keeps_substituted_member_scope() {
    const SRC: &str = "abstract class A<T>(val t: T) { fun get(): T = t }\n\
        private fun <T> make(t: T) = object : A<T>(t) {}\n\
        fun box(): String = make(\"OK\").get()\n";
    assert_eq!(run(SRC).expect("escaping anonymous object member"), "OK");
}

#[test]
fn local_class_unions_overloads_from_all_direct_supertypes() {
    const SRC: &str = "interface Strings { fun choose(value: String) = value }\n\
        interface Ints { fun choose(value: Int) = value.toString() }\n\
        fun box(): String {\n\
        \x20 class Local : Strings, Ints\n\
        \x20 val local = Local()\n\
        \x20 return local.choose(\"O\") + local.choose(1)\n\
        }\n";
    assert_eq!(run(SRC).expect("local supertype overload union"), "O1");
}

#[test]
fn inherited_member_callable_reference_uses_the_complete_local_hierarchy() {
    const SRC: &str = "open class A { fun ok() = \"OK\" }\n\
        open class B : A()\n\
        fun box(): String { class Local : B(); return (Local()::ok)() }\n";
    assert_eq!(run(SRC).expect("local inherited callable reference"), "OK");
}

#[test]
fn nearer_local_override_replaces_its_slot_without_hiding_inherited_overloads() {
    const SRC: &str = "open class Base {\n\
        \x20 open fun choose(value: Int) = \"base:$value\"\n\
        \x20 fun choose(value: String) = value\n\
        }\n\
        fun box(): String {\n\
        \x20 class Local : Base() { override fun choose(value: Int) = value.toString() }\n\
        \x20 val local = Local()\n\
        \x20 return local.choose(\"O\") + local.choose(1)\n\
        }\n";
    assert_eq!(run(SRC).expect("local override family"), "O1");
}
