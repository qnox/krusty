//! Lexical-scope extents the checker must get exactly right.
//!
//! The checker's scope chain links each scope to its parent by borrow, so a scope's extent is the
//! Rust block it lives in. That makes the closing brace load-bearing: placed one statement too
//! late, a receiver or a flow proof leaks into code that must not see it, and the result is an
//! accepted program that miscompiles rather than a diagnostic. Every case here was verified
//! against the reference `kotlinc` (2.4.10) first — the comment on each test records its verdict.

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

/// kotlinc: `error: initializer type mismatch: expected 'Int', actual 'Int?'`.
///
/// A straight-line narrowing (`var x: Int? = 10` reads as `Int`) is proven in one scope, but an
/// assignment ANYWHERE — including inside a nested block — disproves it. Invalidation therefore
/// walks the whole chain even though proving is scope-local. Reading only the current scope is not
/// enough: the branch's own frame is empty, so clearing just that one leaves the outer proof alive
/// and `val y: Int = x` unboxes null at runtime.
#[test]
fn an_assignment_in_a_nested_block_invalidates_an_outer_narrowing() {
    const SRC: &str = "fun f(cond: Boolean): Int {\n\
        \x20   var x: Int? = 10\n\
        \x20   if (cond) { x = null }\n\
        \x20   val y: Int = x\n\
        \x20   return y\n\
        }\n\
        fun box(): String = \"OK\"\n";
    assert_rejected(SRC);
}

/// kotlinc: accepted.
///
/// Checking a member EXTENSION property hides the class's own properties so they cannot shadow the
/// extension receiver, then restores them. The restore must land in the CLASS BODY scope they were
/// taken from — if the accessor scope is still open, they are restored into a scope that dies
/// immediately and the class loses them, including plain (non-property) constructor parameters
/// that no member channel can recover.
#[test]
fn a_member_extension_property_does_not_consume_the_class_scope() {
    const SRC: &str = "class C(n: Int) {\n\
        \x20   val String.tag: Int get() = length\n\
        \x20   val doubled: Int = n * 2\n\
        \x20   init { require(n >= 0) }\n\
        }\n\
        fun box(): String = if (C(3).doubled == 6) \"OK\" else \"FAIL\"\n";
    assert_eq!(run(SRC).expect("ctor param stays visible"), "OK");
}

/// kotlinc: `error: unresolved reference 'inst'`.
///
/// A companion object's members are emitted statically, so they have no enclosing instance: the
/// class rung must close before the companion section.
#[test]
fn a_companion_body_has_no_enclosing_instance() {
    const SRC: &str = "class C {\n\
        \x20   fun inst(): Int = 1\n\
        \x20   companion object {\n\
        \x20       fun stat(): Int = inst()\n\
        \x20   }\n\
        }\n\
        fun box(): String = \"OK\"\n";
    assert_rejected(SRC);
}

/// kotlinc: `error: initializer type mismatch: expected 'Int', actual 'Int?'`.
///
/// A property-path proof (`a.v` non-null) belongs to the binding of `a` it was proven against. A
/// `for` loop re-declaring `a` shadows that binding, so the outward walk must stop at the rung
/// declaring the root — consulting one rung further finds the shadowed binding's proof and unboxes
/// null.
#[test]
fn a_shadowing_loop_variable_does_not_inherit_the_outer_path_proof() {
    const SRC: &str = "class Box(val v: Int?)\n\
        fun f(a: Box, list: List<Box>): Int {\n\
        \x20   if (a.v == null) return 0\n\
        \x20   var total = 0\n\
        \x20   for (a in list) {\n\
        \x20       val s: Int = a.v\n\
        \x20       total += s\n\
        \x20   }\n\
        \x20   return total\n\
        }\n\
        fun box(): String = \"OK\"\n";
    assert_rejected(SRC);
}

/// kotlinc: `error: unresolved reference 'tag'`.
///
/// `this` is the extension receiver only inside an extension property's ACCESSORS. The delegate
/// expression is evaluated once, receiver-less, at file initialization, so the receiver rung must
/// close before it.
#[test]
fn an_extension_property_delegate_has_no_receiver() {
    const SRC: &str = "class Recv { fun tag(): String = \"r\" }\n\
        class Del { operator fun getValue(r: Any?, p: Any?): String = \"d\" }\n\
        fun mk(s: String) = Del()\n\
        val Recv.foo: String by mk(tag())\n\
        fun box(): String = \"OK\"\n";
    assert_rejected(SRC);
}

/// kotlinc: accepted.
///
/// A default argument is evaluated in the caller's context, so no sibling parameter is in scope —
/// but the generated `f$default` DOES receive the extension receiver, so `this` stays available.
/// The rule differs from a CONSTRUCTOR default, which has no receiver at all.
///
/// Asserted at the FRONT END only: lowering cannot yet emit a `$default` stub that threads the
/// extension receiver, so the file still bails in the IR backend.
#[test]
fn an_extension_function_default_argument_sees_the_receiver() {
    const SRC: &str = "fun String.f(n: Int = this.length): Int = n\n\
        fun box(): String = if (\"abc\".f() == 3) \"OK\" else \"FAIL\"\n";
    let stdlib = common::stdlib_jar();
    let jdk = common::jdk_modules();
    let diagnostics = common::front_end_diagnostics(SRC, &[stdlib], Some(jdk.as_path()));
    assert!(
        !diagnostics
            .iter()
            .any(|d| d.contains("'this' is not available")),
        "`this` must resolve to the extension receiver in a default argument, got: {diagnostics:?}"
    );
}

/// kotlinc: `error: cannot use 'T' as reified type parameter`.
///
/// A declaration's type parameters must be retired with the declaration. Declaring them into the
/// caller's scope — the shared file root, for a top-level function — leaves every later
/// declaration in the file seeing them, and `T::class` on a plain (non-reified) parameter then
/// emits a constant-pool class reference to a type that does not exist.
#[test]
fn a_reified_type_parameter_does_not_leak_into_later_declarations() {
    const SRC: &str = "inline fun <reified T> a(): String = T::class.java.simpleName\n\
        fun <T> b(x: T): String = T::class.java.simpleName\n\
        fun box(): String = \"OK\"\n";
    assert_rejected(SRC);
}

/// kotlinc: `error: unresolved reference 'T'`.
///
/// Same rule one level down: a member property's own type parameters must not survive into the
/// rest of the class body, including `init` blocks and later body properties.
#[test]
fn a_member_property_type_parameter_does_not_leak_into_the_class_body() {
    const SRC: &str = "class C {\n\
        \x20   val <T> List<T>.head: T get() = this[0]\n\
        \x20   init {\n\
        \x20       val z: T? = null\n\
        \x20       println(z)\n\
        \x20   }\n\
        }\n\
        fun box(): String = \"OK\"\n";
    assert_rejected(SRC);
}

/// kotlinc: accepted.
///
/// An assignment invalidates a straight-line narrowing chain-wide, but the frame is keyed by NAME:
/// the walk must stop at the rung declaring that name, or assigning to a shadowing variable that
/// merely shares it destroys the enclosing binding's proof.
#[test]
fn assigning_to_a_shadowing_variable_keeps_the_outer_narrowing() {
    const SRC: &str = "fun f(c: Boolean): Int {\n\
        \x20   var x: Int? = null\n\
        \x20   x = 10\n\
        \x20   if (c) {\n\
        \x20       var x: String? = null\n\
        \x20       x = null\n\
        \x20   }\n\
        \x20   val y: Int = x\n\
        \x20   return y\n\
        }\n\
        fun box(): String = if (f(true) == 10) \"OK\" else \"FAIL\"\n";
    assert_eq!(run(SRC).expect("outer narrowing survives"), "OK");
}

/// kotlinc: `error: unresolved reference 'T'.`
///
/// A plain nested class does not carry its outer instance, and the same cut applies to the outer
/// declaration's TYPE parameters: `A<T>`'s `T` is unreachable from `A.B`. Type parameters are
/// scope-chain bindings in the classifier namespace, so the cut is the one rung walk — not a
/// separate rule.
#[test]
fn a_nested_class_cannot_name_the_outer_classs_type_parameter() {
    const SRC: &str = "class A<T> {\n\
        \x20   class B {\n\
        \x20       fun g(): T? = null\n\
        \x20   }\n\
        }\n\
        fun box(): String = \"OK\"\n";
    assert_rejected(SRC);
}

/// kotlinc: accepted.
///
/// An `inner class` keeps the enclosing instance, so it keeps `T` too — the same rung that carries
/// `this@A` carries the type parameters declared on it.
#[test]
fn an_inner_class_can_name_the_outer_classs_type_parameter() {
    const SRC: &str = "class A<T>(val t: T) {\n\
        \x20   inner class C {\n\
        \x20       fun h(): T = t\n\
        \x20   }\n\
        }\n\
        fun box(): String = if (A(\"s\").C().h() == \"s\") \"OK\" else \"FAIL\"\n";
    assert_eq!(run(SRC).expect("inner class keeps T"), "OK");
}

/// kotlinc: `error: unresolved reference 'T'.`
///
/// A declaration's type parameters retire with the rung that declared them. Binding them into the
/// shared enclosing scope instead would leave them visible to every later declaration in the file,
/// so `g` would silently accept `T` — a name that does not exist there.
#[test]
fn a_type_parameter_does_not_leak_to_the_next_declaration() {
    const SRC: &str = "fun <T> f(t: T): T = t\n\
        fun g(): T? = null\n\
        fun box(): String = \"OK\"\n";
    assert_rejected(SRC);
}

/// kotlinc: `error: cannot use 'T' as reified type parameter. Use a class instead.`
///
/// `reified` is a property OF the type-parameter binding, not a parallel set: an `inline fun
/// <reified T>` cannot leave `T` reified for a later declaration that merely reuses the name.
#[test]
fn a_reified_mark_does_not_leak_to_the_next_declaration() {
    const SRC: &str = "inline fun <reified T> f(): String = T::class.java.name\n\
        fun <T> g(): String = T::class.java.name\n\
        fun box(): String = \"OK\"\n";
    assert_rejected(SRC);
}
