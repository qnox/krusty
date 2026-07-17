//! An enum class may declare body member properties (`enum class E(val a: Int) { X(3); val b = a*2 }`)
//! — a backing field on the enum class initialized in its constructor. The parser accepts a `val`/`var`
//! body member, and the enum emitter declares the field and runs its initializer in the constructor.
//! Same-file, runnable.
use super::common;

fn run(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, "Main")
}

#[test]
fn unqualified_nested_type_field_resolves() {
    // A field typed by an UNQUALIFIED reference to the enclosing class's nested type (`val inner: Inner`
    // inside `Outer`) must resolve to `Outer$Inner`, not erase to `Object` — its getter/componentN/copy
    // and the ctor param all carry the concrete type (Kotlin's nested-type scoping).
    const SRC: &str = "data class Outer(val inner: Inner, val n: Int) {\n\
        \x20 data class Inner(val x: Int)\n\
        }\n\
        fun box(): String {\n\
        \x20 val o = Outer(Outer.Inner(7), 1)\n\
        \x20 return if (o.inner.x == 7 && o.component1().x == 7) \"OK\" else \"fail\"\n\
        }\n";
    assert_eq!(run(SRC).expect("unqualified nested-type field"), "OK");
}

#[test]
fn enum_ctor_property_private_field_read_through_getter() {
    // kotlinc emits an enum ctor property as a PRIVATE field + `getX()`. A bodied-entry override (a
    // subclass) AND a cross-class reader must read it through the getter, not a `getfield` on the now-
    // private field (which would be an `IllegalAccessError` — box `enum/kt2350`).
    const SRC: &str = "enum class A(val b: String) {\n\
        \x20 E1(\"e1\") { override fun t() = b },\n\
        \x20 E2(\"e2\") { override fun t() = b.uppercase() };\n\
        \x20 abstract fun t(): String\n\
        }\n\
        class Reader { fun read(a: A): String = a.b }\n\
        fun box(): String =\n\
        \x20 if (A.E1.t() == \"e1\" && A.E2.t() == \"E2\" && Reader().read(A.E1) == \"e1\") \"OK\" else \"fail\"\n";
    assert_eq!(run(SRC).expect("enum private-field getter routing"), "OK");
}

#[test]
fn enum_body_property_reads_ctor_param() {
    const SRC: &str = "enum class E(val a: Int) {\n\
        \x20 X(3),\n\
        \x20 Y(5);\n\
        \x20 val b = a * 2\n\
        }\n\
        fun box(): String =\n\
        \x20 if (E.X.b == 6 && E.Y.b == 10) \"OK\" else \"fail\"\n";
    assert_eq!(run(SRC).expect("enum body property"), "OK");
}

#[test]
fn enum_body_const_property() {
    const SRC: &str = "enum class E {\n\
        \x20 A, B;\n\
        \x20 val b = 42\n\
        }\n\
        fun box(): String = if (E.A.b == 42 && E.B.b == 42) \"OK\" else \"fail\"\n";
    assert_eq!(run(SRC).expect("enum body const property"), "OK");
}

#[test]
fn enum_body_var_property() {
    const SRC: &str = "enum class E {\n\
        \x20 A, B;\n\
        \x20 var hits = 0\n\
        }\n\
        fun box(): String {\n\
        \x20 E.A.hits = 7\n\
        \x20 return if (E.A.hits == 7 && E.B.hits == 0) \"OK\" else \"fail\"\n\
        }\n";
    assert_eq!(run(SRC).expect("enum body var property"), "OK");
}
