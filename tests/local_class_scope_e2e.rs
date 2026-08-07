//! A local class is checked in the LEXICAL SCOPE it was written in.
//!
//! It used to be hoisted to a top-level declaration and checked with no enclosing scope at all, so
//! every reference to anything outside it failed to resolve and the file skipped. It is now entered
//! from its `Stmt::LocalClass`, on a class rung that carries the enclosing instance — which makes
//! the enclosing declaration's type parameters and receivers reachable, exactly as kotlinc has them.
//!
//! Capture of an enclosing VALUE follows from that: what the class reads is decided in the scope it
//! was written in, and lowering carries each captured binding as a leading constructor parameter.
//! A reference that is not modelled yet (the enclosing INSTANCE, a local function, a reassigned
//! `var`, or a capture read during construction) is rejected — the file skips rather than emitting a
//! class without what it needs. Each test records the reference `kotlinc` (2.4.10) verdict.

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

/// kotlinc: accepted.
///
/// The local class reads a local of the enclosing function. The lexical scope is what resolves it;
/// the captured binding then reaches the instance as a leading constructor parameter, supplied at
/// the construction site.
#[test]
fn a_local_class_captures_an_enclosing_local() {
    const SRC: &str = "fun f(): String {\n\
        \x20   val captured = \"OK\"\n\
        \x20   class L { fun read() = captured }\n\
        \x20   return L().read()\n\
        }\n\
        fun box(): String = f()\n";
    assert_eq!(run(SRC).expect("captured local is carried"), "OK");
}

/// kotlinc: accepted.
///
/// Several captures plus the class's own constructor argument: the captures come FIRST and the
/// source argument after, so the two cannot be confused for one another.
#[test]
fn captures_precede_the_classs_own_constructor_arguments() {
    const SRC: &str = "fun f(): String {\n\
        \x20   val a = \"O\"\n\
        \x20   val b = \"K\"\n\
        \x20   class L(val own: String) { fun read() = a + b + own }\n\
        \x20   return L(\"!\").read()\n\
        }\n\
        fun box(): String = if (f() == \"OK!\") \"OK\" else \"FAIL\"\n";
    assert_eq!(run(SRC).expect("captures precede source arguments"), "OK");
}

/// kotlinc: accepted.
///
/// A capture read from a parameter of the enclosing function, constructed more than once — each
/// construction supplies the value from the frame it runs in.
#[test]
fn a_captured_parameter_is_supplied_at_every_construction() {
    const SRC: &str = "fun f(p: String): String {\n\
        \x20   class L { fun read() = p }\n\
        \x20   return L().read() + L().read()\n\
        }\n\
        fun box(): String = if (f(\"O\") == \"OO\") \"OK\" else \"FAIL\"\n";
    assert_eq!(run(SRC).expect("every construction supplies it"), "OK");
}

/// kotlinc: accepted — krusty limitation, the file skips.
///
/// A captured `var` that is reassigned is shared MUTABLE state: copying it into the instance would
/// freeze the value at construction, so only an effectively-immutable binding is captured.
#[test]
fn a_reassigned_captured_var_is_rejected() {
    const SRC: &str = "fun f(): String {\n\
        \x20   var captured = \"no\"\n\
        \x20   class L { fun read() = captured }\n\
        \x20   captured = \"OK\"\n\
        \x20   return L().read()\n\
        }\n\
        fun box(): String = f()\n";
    assert_rejected(SRC);
}

/// kotlinc: accepted — krusty limitation, the file skips.
///
/// The capture is read during CONSTRUCTION — here from a primary-constructor parameter default,
/// which is evaluated in a synthetic constructor that carries no captures at all. Scanning only
/// member bodies missed this, and the box corpus caught the miscompile
/// (`localClasses/capturingInDefaultConstructorParameter.kt`).
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
/// Reaching the enclosing INSTANCE is a second capture kind: the receiver itself, not a binding in
/// the chain. Not modelled yet, so a member read through the implicit outer receiver is rejected.
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
