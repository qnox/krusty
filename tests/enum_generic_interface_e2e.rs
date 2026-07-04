//! `enum class E : A<T>` — an enum implementing a GENERIC interface. The enum-level override of the
//! generic method (`override fun foo(t: String)`) gets the erased bridge (`foo(Object)`→`foo(String)`)
//! the JVM needs to dispatch an interface-typed call. (A generic method satisfied only by per-entry
//! overrides skips — the bridge would belong on each entry subclass.) Round-tripped via the INTERFACE.

use super::common;

fn run(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, "Z")
}

#[test]
fn enum_generic_interface_default_overridden() {
    const SRC: &str = "interface A<T> { open fun foo(t: T): String = \"A\" }\n\
enum class Z(val aname: String) : A<String> {\n\
    Z1(\"Z1\"), Z2(\"Z2\");\n\
    override fun foo(t: String) = aname\n\
}\n\
fun box(): String {\n\
    return when {\n\
        Z.Z1.foo(\"\") != \"Z1\" -> \"Fail #1\"\n\
        (Z.Z1 as A<String>).foo(\"\") != \"Z1\" -> \"Fail #2\"\n\
        (Z.Z2 as A<String>).foo(\"\") != \"Z2\" -> \"Fail #3\"\n\
        else -> \"OK\"\n\
    }\n\
}\n";
    assert_eq!(
        run(SRC).expect("enum generic-interface compiles + runs"),
        "OK"
    );
}

#[test]
fn enum_generic_interface_abstract_method() {
    const SRC: &str = "interface Box<T> { fun get(): T }\n\
enum class Z : Box<String> { Z1; override fun get() = \"OK\" }\n\
fun box(): String = (Z.Z1 as Box<String>).get()\n";
    assert_eq!(
        run(SRC).expect("enum generic-interface abstract compiles + runs"),
        "OK"
    );
}
