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
