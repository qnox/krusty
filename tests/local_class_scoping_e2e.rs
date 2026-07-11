//! Same-named local classes in DIFFERENT functions no longer collide: each is given a unique name and
//! its construction references are rewritten, so the first's members resolve correctly. Runnable.
use super::common;
fn run(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, "Main")
}

#[test]
fn same_named_local_classes_in_different_functions() {
    const SRC: &str = "fun f1(): String {\n\
        \x20 class A(val x: String) { fun g() = x }\n\
        \x20 return A(\"O\").g()\n\
        }\n\
        fun f2(): String {\n\
        \x20 class A(val y: String) { fun g() = y + \"!\" }\n\
        \x20 return A(\"K\").g()\n\
        }\n\
        fun box(): String = if (f1() == \"O\" && f2() == \"K!\") \"OK\" else \"fail\"\n";
    assert_eq!(run(SRC).expect("same-named local classes"), "OK");
}

#[test]
fn local_class_in_class_method() {
    const SRC: &str = "class Outer {\n\
        \x20 fun m1(): String { class L(val v: String) { fun r() = v }; return L(\"O\").r() }\n\
        \x20 fun m2(): String { class L(val w: String) { fun r() = w }; return L(\"K\").r() }\n\
        }\n\
        fun box(): String {\n\
        \x20 val o = Outer()\n\
        \x20 return if (o.m1() == \"O\" && o.m2() == \"K\") \"OK\" else \"fail\"\n\
        }\n";
    assert_eq!(run(SRC).expect("local class in method"), "OK");
}

#[test]
fn shadowing_binding_is_not_clobbered() {
    // A `for` loop var named the same as a (colliding) local class must NOT be rewritten — the pass
    // leaves such a class unrenamed (the file compiles correctly or skips, never miscompiles).
    const SRC: &str = "fun f1(): String {\n\
        \x20 class B(val x: String) { fun g() = x }\n\
        \x20 return B(\"O\").g()\n\
        }\n\
        fun f2(): String {\n\
        \x20 var acc = \"\"\n\
        \x20 for (B in listOf(\"K\")) { acc += B }\n\
        \x20 return acc\n\
        }\n\
        fun box(): String = if (f1() == \"O\" && f2() == \"K\") \"OK\" else \"fail\"\n";
    assert_eq!(run(SRC).expect("shadowing binding"), "OK");
}
