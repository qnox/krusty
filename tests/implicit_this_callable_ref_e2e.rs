//! Implicit-`this` member-PROPERTY callable references (`::prop` inside a class, i.e. `this::prop`).
//!
//! The checker already resolves an unqualified `::m` member-FUNCTION ref inside a class; a member
//! property ref (`::x`) was rejected ("callable references are not supported") purely because the
//! lowerer had no arm for it — the checker/lowerer are deliberately kept in lock-step. The lowering
//! mirrors `lower_implicit_this_method_ref`: capture `this` (slot 0) and dispatch the accessor on it.
//!
//! Safety contract (never miscompile): the checker types `::p` only where lowering can capture its
//! semantic `this` — including class and extension bodies — NOT in a super-constructor argument
//! (uninitialized `this`), not for an outer `this` of an inner class, not for a
//! `private`/`protected` property or a `private`-setter `var` (the synthetic reference class can't
//! reach the accessor, incl. inherited via same-file base-class prop flattening), and not for a
//! computed (backing-field-less) member property.

use super::common;

fn run_box(src: &str, stem: &str) {
    let Some(out) = common::compile_and_run_with_stdlib(src, stem) else {
        panic!("{stem}: expected the box to compile and run");
    };
    assert_eq!(out, "OK", "{stem}");
}

/// `::x` in a member body — a bound `KProperty0` whose `get` dispatches on `this`.
#[test]
fn member_prop_ref_get() {
    run_box(
        r#"
class C {
    val x: String = "OK"
    fun ref() = ::x
}
fun box(): String = C().ref().get()
"#,
        "ThisPropGet",
    );
}

/// `::x` on a `var` — a `KMutableProperty0` whose `set` writes through `this`.
#[test]
fn member_prop_ref_mutable() {
    run_box(
        r#"
class C {
    var x: String = "O"
    fun bump() {
        val r = ::x
        r.set(r.get() + "K")
    }
}
fun box(): String {
    val c = C()
    c.bump()
    return c.x
}
"#,
        "ThisPropMutable",
    );
}

/// `::a` naming an INHERITED member property (the field lives on the base class — the reference
/// dispatches the inherited accessor on `this`).
#[test]
fn inherited_member_prop_ref() {
    run_box(
        r#"
open class A {
    val a: String = "OK"
}
class C: A() {
    fun ref() = ::a
}
fun box(): String = C().ref().get()
"#,
        "ThisPropInherited",
    );
}

/// Known boundaries of THIS feature (promote these to box-OK pins when the machinery lands):
/// delegation BY a callable reference (`val b by ::a`) isn't modeled even for the pre-existing
/// `this::a`/`A()::a` forms (the delegate's `getValue` operator on `KProperty0` isn't resolved).
#[test]
fn delegate_boundary_stays_skipped() {
    if !common::corpus_ready() {
        return;
    }
    let case = "delegatedProperty/delegatedByExtensionMemberProperty.kt";
    assert_eq!(
        common::run_box_corpus_case(case),
        None,
        "{case} needs machinery outside this feature — must stay skipped"
    );
}

/// `::prop.isInitialized` IS resolved now — on a property reference to a `lateinit var` of the
/// enclosing class it is a null check on the backing field, which needs no `KProperty` value.
#[test]
fn lateinit_is_initialized_runs() {
    if !common::corpus_ready() {
        return;
    }
    assert_eq!(
        common::run_box_corpus_case("lateinit/isInitialized.kt").as_deref(),
        Some("OK")
    );
}

/// REJECTION GUARDS for the shadow/visibility/access holes around this feature — each of these
/// compiled-but-crashed (or silently bound the wrong property) without its gate. They must NOT
/// compile (a skip, never a miscompile).
#[test]
fn unsafe_member_prop_ref_shapes_still_rejected() {
    if !common::stdlib_toolchain_ready() {
        return;
    }
    // (name, source) — every one must fail to compile-and-run.
    let cases: &[(&str, &str)] = &[
        // A private member property: no accessor is emitted, so the reference class's `get()`
        // would hit a NoSuchMethodError.
        (
            "PrivateProp",
            r#"
class C {
    private val x: String = "OK"
    fun r() = ::x
}
fun box(): String = C().r().get()
"#,
        ),
        // A `var` with a private setter: the reference's `set` would dispatch the private setter
        // from a separate class (IllegalAccessError).
        (
            "PrivateSetter",
            r#"
class C {
    var x: String = "O"
        private set
    fun r() = ::x
}
fun box(): String {
    val c = C()
    c.r().set("K")
    return c.x
}
"#,
        ),
        // An anonymous object's capture of an enclosing LOCAL (`::x` on a captured local —
        // kotlinc rejects this outright; a top-level capture resolves as the top-level ref,
        // which is sound since the capture IS the top-level static).
        (
            "AnonCapture",
            r#"
fun box(): String {
    val x = "OK"
    val o = object {
        fun use() = x
        fun r() = ::x
    }
    return o.r().get()
}
"#,
        ),
        // A computed member property (no backing field) — the lowerer must bail, never bind a
        // coexisting same-named EXTENSION property's getter.
        (
            "ComputedPlusExtension",
            r#"
class C {
    val x: Int
        get() = 1
    fun r() = ::x
}
val C.x: String
    get() = "ext"
fun box(): String = C().r().get().toString()
"#,
        ),
        // A computed member property shadowing a same-named TOP-LEVEL property — likewise must
        // bail rather than bind the top-level.
        (
            "ComputedPlusTopLevel",
            r#"
val x: String = "top"
class C {
    val x: Int
        get() = 1
    fun r() = ::x
}
fun box(): String = C().r().get().toString()
"#,
        ),
        // A private member property INHERITED from a same-file base (collect flattens base props
        // into the subclass — visibility must still resolve to the DECLARING class).
        (
            "PrivateBaseProp",
            r#"
open class B { private val x: String = "OK" }
class C : B() {
    fun r() = ::x
}
fun box(): String = C().r().get()
"#,
        ),
        // A `var` with a private setter inherited from a same-file base.
        (
            "PrivateBaseSetter",
            r#"
open class B {
    var x: String = "O"
        private set
}
class C : B() {
    fun bump() {
        val r = ::x
        r.set("K")
    }
}
fun box(): String {
    val c = C()
    c.bump()
    return c.x
}
"#,
        ),
        // The same through a GRAND-base (the flattening chains).
        (
            "PrivateGrandBaseProp",
            r#"
open class G { private val x: String = "OK" }
open class B : G()
class C : B() {
    fun r() = ::x
}
fun box(): String = C().r().get()
"#,
        ),
    ];
    for (stem, src) in cases {
        assert!(
            common::compile_and_run_with_stdlib(src, stem).is_none(),
            "{stem}: unsafe ::prop shape must stay rejected (skip, never miscompile)"
        );
    }
}

/// REJECTION GUARD: `::x` in a super-constructor argument must NOT compile — `this` isn't
/// initialized there, so capturing it would emit an uninitialized-receiver read (VerifyError).
#[test]
fn member_prop_ref_in_super_ctor_arg_still_rejected() {
    let src = r#"
open class Base(val r: Any)

class C : Base {
    val x: String = "OK"
    constructor() : super(::x)
}

fun box(): String = "OK"
"#;
    assert!(
        common::compile_and_run_with_stdlib(src, "ThisPropSuperArg").is_none(),
        "::x in a super-constructor argument must stay rejected"
    );
}

/// `this_unavailable` is a callable-reference invariant, not a property-only exception. Method refs
/// in constructor headers must reject the same unavailable/uninitialized semantic receiver.
#[test]
fn implicit_this_method_refs_share_dispatch_receiver_guards() {
    if !common::stdlib_toolchain_ready() {
        return;
    }
    let cases = [
        (
            "MethodSuperArg",
            r#"
open class Base(val r: Any)
class C : Base {
    fun value(): String = "OK"
    constructor() : super(::value)
}
fun box(): String = "OK"
"#,
        ),
        (
            "MethodCtorDefault",
            r#"
class C(val r: () -> String = ::value) {
    fun value(): String = "OK"
}
fun box(): String = C().r()
"#,
        ),
    ];
    for (stem, source) in cases {
        assert!(
            common::compile_and_run_with_stdlib(source, stem).is_none(),
            "{stem}: an implicit method ref without a matching initialized dispatch receiver must skip"
        );
    }
}

/// Implicit refs capture the semantic `this`, not a hard-coded JVM slot. A member extension places
/// its dispatch receiver in slot zero and its extension receiver after it; both function and property
/// references must bind the latter. A top-level extension still places that receiver in slot zero,
/// exercising the same checker-to-lowerer handoff with a different physical layout.
#[test]
fn implicit_refs_capture_the_scoped_extension_receiver() {
    run_box(
        r#"
class Extension(val value: String) {
    fun read(): String = value
}
class Dispatch {
    val value: String = "wrong-property"
    fun read(): String = "wrong-method"
    fun Extension.both(): String = ::value.get() + ::read()
    fun test(): String = Extension("O").both()
}
fun box(): String = if (Dispatch().test() == "OO") "OK" else "FAIL"
"#,
        "ImplicitRefsMemberExtensionReceiver",
    );

    run_box(
        r#"
class Extension(val value: String) {
    fun read(): String = value
}
fun Extension.both(): String = ::value.get() + ::read()
fun box(): String = if (Extension("O").both() == "OO") "OK" else "FAIL"
"#,
        "ImplicitRefsTopLevelExtensionReceiver",
    );
}

/// REJECTION GUARD: an inner class's unqualified `::p` naming an OUTER-class property must NOT
/// compile (the outer `this` isn't capturable here) — and mixed shapes like
/// callableReference/bound/emptyLHS.kt (other extension-ref and outer-this shapes) stay skipped.
#[test]
fn outer_this_and_extension_ref_shapes_still_rejected() {
    let src = r#"
class Outer {
    val x: String = "OK"
    inner class Inner {
        fun ref() = ::x
    }
}
fun box(): String = "OK"
"#;
    assert!(
        common::compile_and_run_with_stdlib(src, "ThisPropOuter").is_none(),
        "an outer-this member property ref must stay rejected"
    );
    if !common::corpus_ready() {
        return;
    }
    assert_eq!(
        common::run_box_corpus_case("callableReference/bound/emptyLHS.kt"),
        None,
        "emptyLHS uses shapes outside this feature — must stay skipped"
    );
}
