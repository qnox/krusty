//! Two related property fixes:
//! 1. A bare `get` / `set` accessor (no parens/body) is the explicit DEFAULT accessor — parseable.
//! 2. A `var` property overriding a GENERIC base property needs a synthetic `setX(erased)` bridge
//!    (mirroring the getter bridge) so a write through the supertype reference reaches the override.
use super::common;

fn run(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, "Main")
}

#[test]
fn bare_get_and_set_accessors_parse() {
    const SRC: &str = "class C {\n\
        \x20 private var a = 1\n\
        \x20   get\n\
        \x20   set\n\
        \x20 val b = 2\n\
        \x20   get\n\
        \x20 fun run(): Int { a = 5; return a + b }\n\
        }\n\
        fun box(): String = if (C().run() == 7) \"OK\" else \"fail\"\n\
        fun main() { println(box()) }\n";
    assert_eq!(run(SRC).expect("bare accessors"), "OK");
}

#[test]
fn generic_var_override_setter_bridge() {
    // Writing `a.x` through the generic supertype reference must reach `B`'s overriding setter.
    const SRC: &str = "open class A<T> {\n\
        \x20 open var x: T = \"Fail\" as T\n\
        }\n\
        class B : A<String>() {\n\
        \x20 override var x: String = \"Fail\"\n\
        }\n\
        fun box(): String {\n\
        \x20 val a: A<String> = B()\n\
        \x20 a.x = \"OK\"\n\
        \x20 return a.x\n\
        }\n\
        fun main() { println(box()) }\n";
    assert_eq!(run(SRC).expect("generic var override"), "OK");
}
