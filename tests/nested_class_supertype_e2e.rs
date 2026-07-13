//! A class may extend a plain NESTED class named by its qualified source form (`Outer.Bar`): the
//! nested class is hoisted to a top-level `Outer$Bar` and the subclass's `super(args)` targets it.
//! Covers both a class-nested and an object-nested base (a singleton's nested class is still a plain
//! nested class, not `inner`), and reading an inherited constructor-property field. Same-file, runnable.
use super::common;
fn run(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, "Main")
}

#[test]
fn extends_class_nested_base_reads_inherited_field() {
    const SRC: &str = "class Outer { open class Bar(val bar: String) }\n\
        class Baz: Outer.Bar(\"OK\")\n\
        fun box(): String = Baz().bar\n";
    assert_eq!(run(SRC).expect("class-nested base"), "OK");
}

#[test]
fn extends_object_nested_base_reads_inherited_field() {
    const SRC: &str = "object Foo { open class Bar(val bar: String) }\n\
        class Baz: Foo.Bar(\"OK\")\n\
        fun box(): String = Baz().bar\n";
    assert_eq!(run(SRC).expect("object-nested base"), "OK");
}

#[test]
fn extends_no_arg_nested_base() {
    const SRC: &str = "class Outer { open class Bar { fun tag() = \"OK\" } }\n\
        class Baz: Outer.Bar()\n\
        fun box(): String = Baz().tag()\n";
    assert_eq!(run(SRC).expect("no-arg nested base"), "OK");
}

#[test]
fn nested_class_implements_sibling_nested_interface() {
    // A nested class implements a SIBLING nested interface named by simple name (`Foo`, not
    // `Test.Foo`) — resolved through the enclosing scope. The interface is hoisted and emitted.
    const SRC: &str = "class Test {\n\
        \x20 interface Foo { fun r(): String }\n\
        \x20 class Impl: Foo { override fun r() = \"OK\" }\n\
        }\n\
        fun box(): String = Test.Impl().r()\n";
    assert_eq!(run(SRC).expect("sibling nested iface"), "OK");
}

#[test]
fn nested_class_extends_sibling_nested_class() {
    // A nested class extends a SIBLING nested (open) class by simple name.
    const SRC: &str = "class Test {\n\
        \x20 open class Base(val s: String)\n\
        \x20 class Sub: Base(\"OK\")\n\
        }\n\
        fun box(): String = Test.Sub().s\n";
    assert_eq!(run(SRC).expect("sibling nested base"), "OK");
}
