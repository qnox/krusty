//! A local class is checked in the LEXICAL SCOPE it was written in.
//!
//! It is entered from its `Stmt::LocalClass`, on a class rung that carries the enclosing instance,
//! which makes the enclosing declaration's type parameters and receivers reachable, exactly as
//! kotlinc has them.
//!
//! Capture of an enclosing VALUE follows from that: what the class reads is decided in the scope it
//! was written in, and lowering carries each captured binding as a leading constructor parameter.
//! The enclosing INSTANCE is the second capture kind — the receiver itself rather than a binding in
//! the chain — and is carried as one capture, first, since lowering identifies it by position.
//! A reference that is not modelled yet (a local function, a reassigned `var`, or a capture read
//! during construction) is rejected — the file skips rather than emitting a class without what it
//! needs. Each test records the reference `kotlinc` (2.4.10) verdict.

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

/// kotlinc: accepted.
///
/// Reading a member through the implicit outer receiver captures the enclosing INSTANCE, not the
/// member: one capture however many members are read, and the read goes through it.
#[test]
fn a_local_class_captures_the_enclosing_instance() {
    const SRC: &str = "class Outer {\n\
        \x20   val tag: String = \"O\"\n\
        \x20   fun suffix() = \"K\"\n\
        \x20   fun m(): String {\n\
        \x20       class L { fun read() = tag + suffix() }\n\
        \x20       return L().read()\n\
        \x20   }\n\
        }\n\
        fun box(): String = Outer().m()\n";
    assert_eq!(run(SRC).expect("enclosing instance is carried"), "OK");
}

/// kotlinc: accepted.
///
/// An explicit `this@Outer` reaches the same capture as an unqualified member read.
#[test]
fn a_local_class_captures_a_labeled_enclosing_this() {
    const SRC: &str = "class Outer {\n\
        \x20   val tag: String = \"OK\"\n\
        \x20   fun m(): String {\n\
        \x20       class L { fun read() = this@Outer.tag }\n\
        \x20       return L().read()\n\
        \x20   }\n\
        }\n\
        fun box(): String = Outer().m()\n";
    assert_eq!(run(SRC).expect("labeled outer this is carried"), "OK");
}

/// kotlinc: accepted.
///
/// The enclosing instance and an enclosing local, together: the instance comes FIRST, because
/// lowering finds it at field 0.
#[test]
fn the_enclosing_instance_precedes_a_captured_local() {
    const SRC: &str = "class Outer {\n\
        \x20   val tag: String = \"O\"\n\
        \x20   fun m(): String {\n\
        \x20       val extra = \"K\"\n\
        \x20       class L { fun read() = tag + extra }\n\
        \x20       return L().read()\n\
        \x20   }\n\
        }\n\
        fun box(): String = Outer().m()\n";
    assert_eq!(run(SRC).expect("instance first, then locals"), "OK");
}

/// kotlinc: accepted — krusty limitation, the file skips.
///
/// A `@JvmInline value class` has no instance to capture: `this` is the bare underlying value, so a
/// field typed as the class would hold something else entirely. The box corpus caught this as a
/// `VerifyError` (`codegen/box/inlineClasses/initBlock.kt`).
#[test]
fn capturing_an_unboxed_value_class_receiver_is_rejected() {
    const SRC: &str = "@JvmInline\n\
        value class V(val s: String) {\n\
        \x20   fun m(): String {\n\
        \x20       class L { fun read() = s }\n\
        \x20       return L().read()\n\
        \x20   }\n\
        }\n\
        fun box(): String = V(\"OK\").m()\n";
    assert_rejected(SRC);
}

/// kotlinc: accepted.
///
/// Two local classes of the same name in different bodies. Neither reference is rewritten: each
/// hoisted declaration is named after the declaration it was written in, and the SOURCE name
/// resolves through the scope chain where it stands.
#[test]
fn same_named_local_classes_resolve_to_their_own_declaration() {
    const SRC: &str = "fun f1(): String {\n\
        \x20   class A(val x: String) { fun g() = x }\n\
        \x20   return A(\"O\").g()\n\
        }\n\
        fun f2(): String {\n\
        \x20   class A(val y: String) { fun g() = y + \"K\" }\n\
        \x20   return A(\"\").g()\n\
        }\n\
        fun box(): String = f1() + f2()\n";
    assert_eq!(run(SRC).expect("each resolves to its own"), "OK");
}

/// kotlinc: accepted.
///
/// A local class names a sibling local class as its supertype by the SOURCE name. Signature
/// collection has no scope chain, so it is handed the body's classifiers explicitly.
#[test]
fn a_local_class_inherits_from_a_sibling_local_class() {
    const SRC: &str = "fun box(): String {\n\
        \x20   open class Base { open fun name() = \"FAIL\" }\n\
        \x20   class Derived : Base() { override fun name() = \"OK\" }\n\
        \x20   val b: Base = Derived()\n\
        \x20   return b.name()\n\
        }\n";
    assert_eq!(run(SRC).expect("local supertype resolves"), "OK");
}

/// kotlinc: accepted.
///
/// A local class in an `is`/`as` target. Lowering has no scope chain either — it falls back to the
/// type reference the checker already resolved, keyed by span.
#[test]
fn a_local_class_is_a_type_test_target() {
    const SRC: &str = "fun box(): String {\n\
        \x20   class L(val v: String)\n\
        \x20   val a: Any = L(\"OK\")\n\
        \x20   return if (a is L) (a as L).v else \"FAIL\"\n\
        }\n";
    assert_eq!(run(SRC).expect("is/as on a local class"), "OK");
}

/// kotlinc: accepted.
///
/// An unbound member reference whose receiver is a local class. The AST name names no class after
/// hoisting, so lowering takes the receiver from the reference's own first parameter — the type the
/// checker recorded.
#[test]
fn an_unbound_member_reference_on_a_local_class() {
    const SRC: &str = "fun box(): String {\n\
        \x20   class L { fun foo(): String = \"OK\" }\n\
        \x20   return (L::foo)(L())\n\
        }\n";
    assert_eq!(run(SRC).expect("unbound ref on a local class"), "OK");
}

/// kotlinc: accepted.
///
/// An `inner class` of a local class, constructed through an instance of it. The nested declaration
/// is hoisted during the local class's parse and has to be requalified with it — including the
/// `inner_of` edge its synthetic outer-instance field is typed from.
#[test]
fn an_inner_class_of_a_local_class() {
    const SRC: &str = "fun box(): String {\n\
        \x20   class Outer(val tag: String) {\n\
        \x20       inner class Inner { fun read() = tag }\n\
        \x20   }\n\
        \x20   return Outer(\"OK\").Inner().read()\n\
        }\n";
    assert_eq!(run(SRC).expect("inner class of a local class"), "OK");
}

/// kotlinc: accepted, prints `OK`.
///
/// A local class SHADOWS an enclosing type parameter of the same name: one classifier namespace, so
/// the innermost binding wins whatever kind it is. Stepping over the class binding to keep walking
/// for a type parameter resolved `T` to the parameter and `v.s()` was `unresolved reference 's'`.
#[test]
fn a_local_class_shadows_an_enclosing_type_parameter_of_the_same_name() {
    const SRC: &str = "fun <T> f(): String {\n\
        \x20   class T { fun s(): String = \"OK\" }\n\
        \x20   val v: T = T()\n\
        \x20   return v.s()\n\
        }\n\
        fun box(): String = f<Int>()\n";
    assert_eq!(
        run(SRC).expect("the local class shadows the type parameter"),
        "OK"
    );
}

/// kotlinc: accepted — krusty limitation, the file skips.
///
/// A local class reading a receiver FURTHER OUT than the innermost one would need a chain of
/// captures (`L.this$0` is the `B` instance, and `this@A` is another hop through `B.this$0`), which
/// is not modelled. Only the innermost receiver contributes capture names, so `this@A` finds no
/// binding and the class is rejected instead of being handed a `this$0` that holds the wrong object.
#[test]
fn a_local_class_reading_a_grandparent_receiver_is_rejected() {
    const SRC: &str = "class A(val x: String) {\n\
        \x20   inner class B {\n\
        \x20       fun m(): String {\n\
        \x20           class L { fun k(): String = this@A.x }\n\
        \x20           return L().k()\n\
        \x20       }\n\
        \x20   }\n\
        }\n\
        fun box(): String = A(\"OK\").B().m()\n";
    assert_rejected(SRC);
}

/// kotlinc: accepted — krusty limitation, the file skips.
///
/// Inside a member EXTENSION function the nearest `this` is the extension receiver, while lowering
/// supplies a captured enclosing instance from the DISPATCH receiver. The two are different objects,
/// so the enclosing instance contributes no capture names at all and the read is rejected rather
/// than emitted as a field typed after one receiver holding the other.
#[test]
fn a_local_class_capturing_through_a_member_extension_receiver_is_rejected() {
    const SRC: &str = "class Outer(val n: Int) {\n\
        \x20   fun greet(): String = \"hi\" + n\n\
        \x20   fun String.ext(): String {\n\
        \x20       class L { fun read(): String = greet() }\n\
        \x20       return L().read()\n\
        \x20   }\n\
        \x20   fun run(): String = \"x\".ext()\n\
        }\n\
        fun box(): String = Outer(3).run()\n";
    assert_rejected(SRC);
}

/// kotlinc: accepted, prints `OK`.
///
/// The plain case the two rejections above must not cost: a local class in an ordinary member
/// function, where the innermost `this` IS the dispatch receiver, still captures the enclosing
/// instance and calls its method.
#[test]
fn a_local_class_in_a_plain_member_captures_the_enclosing_instance() {
    const SRC: &str = "class Outer(val n: Int) {\n\
        \x20   fun greet(): String = if (n == 3) \"OK\" else \"FAIL\"\n\
        \x20   fun run(): String {\n\
        \x20       class L { fun read(): String = greet() }\n\
        \x20       return L().read()\n\
        \x20   }\n\
        }\n\
        fun box(): String = Outer(3).run()\n";
    assert_eq!(
        run(SRC).expect("a plain member's local class captures the enclosing instance"),
        "OK"
    );
}
