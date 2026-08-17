//! A classifier nested in an INTERFACE is in scope for the interface's own members, exactly as it
//! is for a class owner (`interface C { class K; fun g(): K? }` — kotlinc accepts). The parser
//! previously hoisted an interface's nested classifier only when it was itself an interface, an
//! annotation, or an implementor of the enclosing interface; a plain nested `class`/`enum class`
//! was silently dropped, so `K` (and even the qualified `C.K` from outside) read as unresolved.
//! This is the exact shape of intellij's `plugins/textmate/core` (`interface Constants { enum class
//! StringKey … }` with members referencing `StringKey`).
use super::common;

fn run(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, "Main")
}

fn assert_byte_identical(name: &str, src: &str, class: &str) {
    match common::byte_diff_against_kotlinc(name, src, class) {
        None => panic!("{name}: reference toolchain unavailable"),
        Some(Ok(())) => {}
        Some(Err(e)) => panic!("{e}"),
    }
}

#[test]
fn interface_member_signature_references_nested_class() {
    // The minimal repro: a member signature naming a sibling nested class by SIMPLE name.
    const SRC: &str = "interface C {\n\
        \x20 class K\n\
        \x20 fun g(): K?\n\
        }\n\
        class Impl : C {\n\
        \x20 override fun g(): C.K? = null\n\
        }\n\
        fun box(): String = if (Impl().g() == null) \"OK\" else \"fail\"\n";
    assert_eq!(
        run(SRC).expect("interface nested-class member signature"),
        "OK"
    );
}

#[test]
fn interface_nested_enum_companion_member_references_enum() {
    // The textmate `Constants` shape: an enum nested in an interface, whose companion members
    // reference the enum by simple name (return type `K?` and `K.entries`).
    const SRC: &str = "interface C {\n\
        \x20 enum class K(val v: String) {\n\
        \x20   A(\"a\");\n\
        \x20   companion object {\n\
        \x20     fun fromName(n: String): K? {\n\
        \x20       for (e in K.entries) {\n\
        \x20         if (e.v == n) return e\n\
        \x20       }\n\
        \x20       return null\n\
        \x20     }\n\
        \x20   }\n\
        \x20 }\n\
        }\n\
        fun box(): String {\n\
        \x20 val a = C.K.fromName(\"a\") ?: return \"missing\"\n\
        \x20 if (C.K.fromName(\"zz\") != null) return \"phantom\"\n\
        \x20 return if (a == C.K.A && a.v == \"a\") \"OK\" else \"fail\"\n\
        }\n";
    assert_eq!(run(SRC).expect("interface nested-enum companion"), "OK");
}

#[test]
fn interface_nested_class_visible_outside_qualified() {
    // A nested classifier referenced from OUTSIDE the interface, by its qualified name.
    const SRC: &str = "interface C {\n\
        \x20 class K {\n\
        \x20   fun tag(): String = \"OK\"\n\
        \x20 }\n\
        }\n\
        fun box(): String = C.K().tag()\n";
    assert_eq!(
        run(SRC).expect("interface nested class qualified use"),
        "OK"
    );
}

#[test]
fn class_owner_nested_enum_regression_anchor() {
    // The CLASS-owner equivalent always worked; it must stay working through the shared path.
    const SRC: &str = "class C {\n\
        \x20 enum class K { A }\n\
        \x20 fun g(): K? = K.A\n\
        }\n\
        fun box(): String = if (C().g() == C.K.A) \"OK\" else \"fail\"\n";
    assert_eq!(run(SRC).expect("class-owner nested enum anchor"), "OK");
}

#[test]
fn interface_nested_class_calls_private_interface_member() {
    // The historical reason interface bodies DROPPED plain nested classes: a nested helper calling a
    // PRIVATE interface member needs a synthetic accessor (kotlinc emits `access$priv`). krusty's
    // `access$` bridge synthesis covers it (as an interface default-method bridge rather than
    // kotlinc's static form — different shape, same semantics), so the call must compile AND run.
    const SRC: &str = "interface B {\n\
        \x20 private fun priv(): String = \"OK\"\n\
        \x20 fun pub(): String\n\
        \x20 class Z {\n\
        \x20   fun f(b: B): String = b.priv()\n\
        \x20 }\n\
        }\n\
        class Impl : B {\n\
        \x20 override fun pub(): String = \"pub\"\n\
        }\n\
        fun box(): String = B.Z().f(Impl())\n";
    assert_eq!(run(SRC).expect("nested class calls private member"), "OK");
}

#[test]
fn interface_nested_class_bytes_match_kotlinc() {
    // Whole-classfile parity for the stable minimal shape: the interface itself and the nested class.
    const SRC: &str = "interface C {\n\
        \x20 class K\n\
        \x20 fun g(): K?\n\
        }\n";
    assert_byte_identical("iface_nested_class", SRC, "C");
    assert_byte_identical("iface_nested_class_inner", SRC, "C$K");
}
