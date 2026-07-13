//! An anonymous object in a class method captures the enclosing class's IMMUTABLE (`val`) properties,
//! so its methods can read them (`class A(val x) { fun foo() = object { fun r() = x } }`). A `val` never
//! changes, so capturing its value at construction is equivalent to `this@A.x`. Same-file, runnable.
use super::common;
fn run(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, "Main")
}

#[test]
fn anon_object_reads_enclosing_val_property() {
    const SRC: &str = "interface T { fun result(): String }\n\
        class A(val x: String) {\n\
        \x20 fun foo() = object : T {\n\
        \x20   fun bar() = x\n\
        \x20   override fun result() = bar() + x\n\
        \x20 }\n\
        }\n\
        fun box(): String = if (A(\"O\").foo().result() == \"OO\") \"OK\" else \"no\"\n";
    assert_eq!(run(SRC).expect("anon reads enclosing val"), "OK");
}

#[test]
fn anon_object_reads_enclosing_body_val() {
    const SRC: &str = "interface T { fun result(): String }\n\
        class A {\n\
        \x20 val x: String = \"OK\"\n\
        \x20 fun foo() = object : T { override fun result() = x }\n\
        }\n\
        fun box(): String = A().foo().result()\n";
    assert_eq!(run(SRC).expect("anon reads enclosing body val"), "OK");
}

#[test]
fn anon_object_plain_backing_body_prop_captured() {
    // A plain immutable BACKING-field body property (no custom getter, explicit type) is captured by
    // value — its value at the anon's construction equals a `this@A.doubled` read (it never changes).
    const SRC: &str = "interface T { fun r(): Int }\n\
        class A(val base: Int) {\n\
        \x20 val doubled: Int = base * 2\n\
        \x20 fun foo() = object : T { override fun r() = doubled }\n\
        }\n\
        fun box(): String = if (A(21).foo().r() == 42) \"OK\" else \"no\"\n";
    assert_eq!(run(SRC).expect("plain backing prop captured"), "OK");
}
