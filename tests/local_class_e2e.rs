//! A local class/interface declared inside a function body (`class L { … }`) is hoisted to a
//! top-level-equivalent class and lowered like any other class. Slice 1: non-capturing local classes
//! (a capturing one — e.g. a super-constructor argument referencing an outer local — stays unsupported
//! and the file skips, never miscompiles). Round-tripped under `-Xverify:all`.

use super::common;

fn run(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, "C")
}

#[test]
fn local_class_basic() {
    const SRC: &str = "fun box(): String {\n\
    class Pair(val a: Int, val b: Int) {\n\
        fun sum() = a + b\n\
    }\n\
    val p = Pair(1, 2)\n\
    if (p.sum() != 3) return \"fail sum\"\n\
    if (p.a != 1 || p.b != 2) return \"fail fields\"\n\
    return \"OK\"\n\
}\n";
    let out = run(SRC).expect("local class should compile + run");
    assert_eq!(out, "OK");
}

#[test]
fn local_data_class_and_interface() {
    // A local `data class` (component/equals via the data-class machinery) and a local `interface`
    // implemented by a local class — all non-capturing (slice 1).
    const SRC: &str = "fun box(): String {\n\
    data class Point(val x: Int, val y: Int)\n\
    val p = Point(3, 4)\n\
    if (p.x + p.y != 7) return \"fail data\"\n\
    if (p != Point(3, 4)) return \"fail eq\"\n\
    interface Greeter { fun hi(): String }\n\
    class En : Greeter { override fun hi() = \"hi\" }\n\
    if (En().hi() != \"hi\") return \"fail iface\"\n\
    return \"OK\"\n\
}\n";
    let out = common::expect_box_run_with_stdlib(SRC, "C");
    assert_eq!(out, "OK");
}

#[test]
fn local_class_inheritance_with_modifiers() {
    // Modifier-prefixed local classes (`open`/`abstract`) and local-class inheritance — none capturing
    // an outer local (slice 2a).
    const SRC: &str = "fun box(): String {\n\
    open class Base { open fun name() = \"base\" }\n\
    class Derived : Base() { override fun name() = \"derived\" }\n\
    if (Base().name() != \"base\") return \"fail base\"\n\
    if (Derived().name() != \"derived\") return \"fail derived\"\n\
    val b: Base = Derived()\n\
    if (b.name() != \"derived\") return \"fail virtual\"\n\
    abstract class Shape { abstract fun area(): Int }\n\
    class Sq(val s: Int) : Shape() { override fun area() = s * s }\n\
    if (Sq(3).area() != 9) return \"fail abstract\"\n\
    return \"OK\"\n\
}\n";
    let out = run(SRC).expect("local class inheritance should compile + run");
    assert_eq!(out, "OK");
}

#[test]
fn named_local_object_is_rejected() {
    // Kotlin permits local anonymous-object expressions, but a named singleton declaration cannot
    // be local. Keep this at the frontend boundary: synthesizing JVM INSTANCE storage for it would
    // accept a program kotlinc rejects.
    const SRC: &str = "fun box(): String {\n\
    object Registry {\n\
        val tag = \"reg\"\n\
        fun describe() = \"id=\" + tag\n\
    }\n\
    if (Registry.describe() != \"id=reg\") return \"fail object\"\n\
    val anon = object { val n = 7 }\n\
    if (anon.n != 7) return \"fail anon\"\n\
    return \"OK\"\n\
}\n";
    assert_eq!(
        common::front_end_diagnostics(SRC, &[common::stdlib_jar()], None),
        ["named object 'Registry' cannot be local. Try to use an anonymous object instead."]
    );
}
