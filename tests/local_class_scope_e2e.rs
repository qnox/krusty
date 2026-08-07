//! A local class is checked in the LEXICAL SCOPE it was written in.
//!
//! It used to be hoisted to a top-level declaration and checked with no enclosing scope at all, so
//! every reference to anything outside it failed to resolve and the file skipped. It is now entered
//! from its `Stmt::LocalClass`, on a class rung that carries the enclosing instance — which makes
//! the enclosing declaration's type parameters and receivers reachable, exactly as kotlinc has them.
//!
//! Capture of an enclosing VALUE is a separate matter: lowering does not yet give a local class the
//! constructor parameters a capture needs, so it is rejected (the file skips) instead of being
//! emitted without them. Each test records the reference `kotlinc` (2.4.10) verdict.

use super::common;

fn run(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, "Main")
}

fn assert_rejected(src: &str) {
    assert!(
        common::compile_and_run_with_stdlib(src, "Main").is_none(),
        "source should be rejected, but compiled successfully:\n{src}"
    );
}

/// kotlinc: accepted.
///
/// A local class inside a member of a generic class can name that class's type parameter — the
/// local class carries the enclosing instance, so the classifier walk does not stop at its rung.
/// Checking it at file level (the old hoist) put it outside `A` entirely and `T` was unresolved.
#[test]
fn a_local_class_can_name_the_enclosing_classs_type_parameter() {
    const SRC: &str = "class A<T> {\n\
        \x20   fun m(): String {\n\
        \x20       class L { fun k(): T? = null }\n\
        \x20       return if (L().k() == null) \"OK\" else \"FAIL\"\n\
        \x20   }\n\
        }\n\
        fun box(): String = A<String>().m()\n";
    assert_eq!(run(SRC).expect("outer type parameter resolves"), "OK");
}

/// kotlinc: accepted.
///
/// The local class's own property shadows a same-named property of the enclosing class, so this is
/// NOT a capture — the conservative capture check must not report one, or the file would skip.
#[test]
fn a_local_class_property_shadows_an_enclosing_member_of_the_same_name() {
    const SRC: &str = "class Outer {\n\
        \x20   val tag: String = \"outer\"\n\
        \x20   fun m(): String {\n\
        \x20       class L(val tag: String) { fun read() = tag }\n\
        \x20       return L(\"OK\").read()\n\
        \x20   }\n\
        }\n\
        fun box(): String = Outer().m()\n";
    assert_eq!(run(SRC).expect("own property shadows the outer one"), "OK");
}

/// kotlinc: accepted — krusty limitation, the file skips.
///
/// The local class reads a local of the enclosing function. Resolving it is what the lexical scope
/// buys; EMITTING it needs a constructor parameter carrying the captured value, which lowering does
/// not synthesize yet. Rejecting is the sound outcome — emitting the class without that parameter
/// produced `NoSuchMethodError` on construction.
#[test]
fn a_local_class_capturing_an_enclosing_local_is_rejected() {
    const SRC: &str = "fun f(): String {\n\
        \x20   val captured = \"OK\"\n\
        \x20   class L { fun read() = captured }\n\
        \x20   return L().read()\n\
        }\n\
        fun box(): String = f()\n";
    assert_rejected(SRC);
}

/// kotlinc: accepted — krusty limitation, the file skips.
///
/// The capture sits in a primary-constructor parameter DEFAULT, evaluated in the synthetic
/// `$default` constructor. Scanning only member bodies missed it, and the box corpus caught the
/// miscompile (`localClasses/capturingInDefaultConstructorParameter.kt`).
#[test]
fn a_capture_in_a_constructor_parameter_default_is_rejected() {
    const SRC: &str = "fun f(): String {\n\
        \x20   val captured = \"OK\"\n\
        \x20   class L(val t: String = captured)\n\
        \x20   return L().t\n\
        }\n\
        fun box(): String = f()\n";
    assert_rejected(SRC);
}

/// kotlinc: accepted — krusty limitation, the file skips.
///
/// Reaching the enclosing INSTANCE needs the same missing constructor parameter as reaching a
/// local, so a member read through the implicit outer receiver is a capture too.
#[test]
fn a_local_class_reading_an_enclosing_member_is_rejected() {
    const SRC: &str = "class Outer {\n\
        \x20   val tag: String = \"OK\"\n\
        \x20   fun m(): String {\n\
        \x20       class L { fun read() = tag }\n\
        \x20       return L().read()\n\
        \x20   }\n\
        }\n\
        fun box(): String = Outer().m()\n";
    assert_rejected(SRC);
}
