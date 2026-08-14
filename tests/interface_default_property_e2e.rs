//! Interface properties with DEFAULT accessors (`interface I { val x: String get() = … }`).
//!
//! The pass-1 interface gate (`is_simple_interface`) rejected ANY property with a getter — but the
//! computed-property machinery is interface-agnostic: pass 1 registers `getX()` for any class kind,
//! pass 2 lowers the body generically, and `emit_interface_class` already emits a bodied method as
//! a real JVM default method. The gate now admits a non-`var`, non-extension, non-private computed
//! property (`is_computed_prop`).
//!
//! Deliberately still gated (never miscompile): `var` with custom accessors, extension properties
//! in interfaces, getters that read `field` (a backing field an interface can't have), and PRIVATE
//! computed properties — pass 1 registers their accessor `public`, so a same-named member on an
//! implementor would hijack the virtual dispatch (accessor privacy isn't modeled on this path).

use super::common;

fn run_box(src: &str, stem: &str) {
    let Some(out) = common::compile_and_run_with_stdlib(src, stem) else {
        panic!("{stem}: expected the box to compile and run");
    };
    assert_eq!(out, "OK", "{stem}");
}

/// The basic shape: an abstract property with a default getter, read through the implementor and
/// through the interface type.
#[test]
fn interface_default_getter_basic() {
    run_box(
        r#"
interface I {
    val x: String
        get() = "OK"
}

class C : I

fun box(): String {
    val c: I = C()
    if (C().x != "OK") return "f1"
    if (c.x != "OK") return "f2"
    return "OK"
}
"#,
        "IfaceDefaultGetter",
    );
}

/// An override in a sub-interface wins (kotlinc's most-specific rule). (The corpus diamond
/// `doubleDiamond.kt` additionally needs an `object` with a base class — the separate
/// `gate:object` feature; pinned as a boundary below.)
#[test]
fn interface_default_getter_diamond_override() {
    run_box(
        r#"
interface A {
    val result: String get() = "Fail"
}

interface C : A {
    override val result: String get() = "OK"
}

class Impl : C

fun box(): String {
    if (Impl().result != "OK") return "f1:${Impl().result}"
    val a: A = Impl()
    if (a.result != "OK") return "f2"
    return "OK"
}
"#,
        "IfaceDiamondOverride",
    );
}

/// A getter computing over an abstract property (an indirectly inherited default).
#[test]
fn interface_default_getter_indirect() {
    run_box(
        r#"
interface A {
    val x: String
}

interface B : A {
    val y: String
        get() = x + "K"
}

class C(override val x: String) : B

fun box(): String {
    if (C("O").y != "OK") return "f1"
    return "OK"
}
"#,
        "IfaceIndirectGetter",
    );
}

/// PRIVATE default METHODS — reachable only from inside the interface's own default methods,
/// never dispatched as a public virtual. (A private COMPUTED PROPERTY is NOT on this path — its
/// accessor would be registered `public`, so it stays gated; see the rejection guards below.)
#[test]
fn interface_private_default_method_remains_supported() {
    run_box(
        r#"
interface Z {

    fun testFun() : String {
        return privateFun()
    }

    private fun privateFun(): String {
        return "OK"
    }
}

object Z2 : Z

fun box() : String {
    return Z2.testFun()
}
"#,
        "IfacePrivateAccessor",
    );
}

/// Module signatures, not a current-file class index, identify the declaring interface. Compile both
/// source orders because the multi-file driver lowers each file separately after collecting symbols:
/// either order must let the use-site class inherit and invoke the sibling file's default getter.
#[test]
fn interface_default_getter_from_sibling_file() {
    const API: &str = r#"
interface SharedDefault {
    val result: String
        get() = "OK"
}
"#;
    const USE_SITE: &str = r#"
class SharedDefaultImpl : SharedDefault

fun box(): String = SharedDefaultImpl().result
"#;

    for sources in [
        &[("SharedDefaultApi", API), ("SharedDefaultUse", USE_SITE)][..],
        &[("SharedDefaultUse", USE_SITE), ("SharedDefaultApi", API)][..],
    ] {
        let Some(out) = common::compile_and_run_files_with_stdlib(sources) else {
            panic!("sibling-file interface default getter must compile in either source order");
        };
        assert_eq!(out, "OK");
    }
}

/// A class member beats an interface default at ANY depth (the JVM maximally-specific rule and
/// the checker's owner selection): the superclass chain is walked before interfaces.
#[test]
fn superclass_member_beats_interface_default() {
    run_box(
        r#"
interface I {
    val x: CharSequence get() = "iface"
}

open class Base {
    val x: String get() = "OK"
}

open class Mid : Base()

class C : Mid(), I

fun box(): String {
    if (C().x != "OK") return "f1:${C().x}"
    return "OK"
}
"#,
        "IfaceSuperBeatsIface",
    );
}

/// Overriding ONE overload doesn't drop the delegated forwarder for the OTHER (name-only matching
/// would skip both → `AbstractMethodError` on the un-overridden overload).
#[test]
fn delegation_forwards_unoverridden_overload() {
    run_box(
        r#"
interface I {
    fun foo(x: Int): Int
    fun foo(x: String): String
}

class D : I {
    override fun foo(x: Int): Int = 0
    override fun foo(x: String): String = "OK"
}

class C(val i: I) : I by i {
    override fun foo(x: Int): Int = 1
}

fun box(): String {
    val c = C(D())
    if (c.foo(0) != 1) return "f1"
    return c.foo("x")
}
"#,
        "IfaceDelegateOverload",
    );
}

/// An explicit override of a GENERIC delegated method has a concrete descriptor (`foo(String)`),
/// while the interface obligation and its bridge use the erased descriptor (`foo(Object)`). The
/// delegation pass must leave that erased slot to the bridge pass; synthesizing a delegate forwarder
/// there would shadow the override for calls made through `I<String>` and return `"delegate"`.
#[test]
fn generic_override_beats_delegated_forwarder_after_erasure() {
    run_box(
        r#"
interface I<T> {
    fun foo(x: T): String
}

class D : I<String> {
    override fun foo(x: String): String = "delegate"
}

class C(val delegate: I<String>) : I<String> by delegate {
    override fun foo(x: String): String = "OK"
}

fun box(): String {
    val throughInterface: I<String> = C(D())
    return throughInterface.foo("x")
}
"#,
        "IfaceGenericDelegateOverride",
    );
}

/// A computed `var` has no interface storage: its getter and setter are ordinary default methods.
/// Both accessors must dispatch through the interface declaration, including a write followed by a
/// read on an implementation that declares no override.
#[test]
fn interface_default_getter_and_setter_run() {
    run_box(
        r#"
interface I {
    var x: String
        get() = "OK"
        set(value) {}
}
class C : I
fun box(): String {
    val value: I = C()
    value.x = "ignored"
    return value.x
}
"#,
        "IfaceVar",
    );
}

/// REJECTION GUARDS: shapes that must never EMIT (they'd miscompile as public defaults). Asserts
/// on the backend outcome, not a run result — a skip and an emitted-but-crashing class both make a
/// run-based check pass, but only the former is acceptable.
#[test]
fn unsupported_interface_property_shapes_still_rejected() {
    let jdk = common::jdk_modules();
    let cases: &[(&str, &str)] = &[
        // An extension property in an interface.
        (
            "IfaceExtProp",
            r#"
interface I {
    val String.x: String
        get() = "OK"
}
class C : I
fun box(): String = C().run { "x".x }
"#,
        ),
        // A getter reading `field` (an interface has no backing field).
        (
            "IfaceFieldRead",
            r#"
interface I {
    val x: String
        get() = field
}
class C : I
fun box(): String = C().x
"#,
        ),
        // A PRIVATE computed property: its accessor would be registered `public`, letting a
        // same-named member on an implementor hijack the virtual dispatch (kotlinc emits the
        // getter private; accessor privacy isn't modeled on this path).
        (
            "IfacePrivateComputed",
            r#"
interface I {
    fun test(): String = x
    private val x: String
        get() = "OK"
}
class C : I
fun box(): String = C().test()
"#,
        ),
        // An ABSTRACT extension property: its accessor takes the receiver as a parameter, a
        // descriptor this path doesn't emit.
        (
            "IfaceAbstractExtProp",
            r#"
interface I {
    val String.x: String
}
class C : I {
    override val String.x: String get() = "OK"
}
fun box(): String = with(C()) { "a".x }
"#,
        ),
    ];
    for (stem, src) in cases {
        let cp = krusty::toolchain::classpath_jars_for(src);
        let outcome = common::backend_outcome_in_process(src, stem, &cp, Some(jdk.as_path()));
        assert_ne!(
            outcome,
            Some(common::BackendOutcome::Emitted),
            "{stem}: unsupported interface property shape must not emit (skip, never miscompile)"
        );
    }
}

/// The exact corpus cases. `traits/doubleDiamond.kt` additionally needs an `object` with a base
/// class (`gate:object`, a separate feature) — pinned as a boundary below.
#[test]
fn corpus_interface_default_getters_box_ok() {
    if !common::corpus_ready() {
        return;
    }
    for case in [
        "traits/indirectlyInheritPropertyGetter.kt",
        // Regression pins: dispatch-order cases the interface-property walk must get right —
        // class-member-beats-interface (kt42137), erased-generic superclass member
        // (covariantGenericDiamond), sub-interface override beats base default
        // (propertyDiamondFakeOverride), own override beats delegated forwarder (kt2532).
        "bridges/kt42137.kt",
        "bridges/covariantGenericDiamond.kt",
        "bridges/propertyDiamondFakeOverride.kt",
        "classes/kt2532.kt",
    ] {
        assert_eq!(
            common::run_box_corpus_case(case).as_deref(),
            Some("OK"),
            "{case} must execute successfully, not silently skip"
        );
    }
}

/// The corpus diamond needs `object CImpl : BImpl(), C` — an object with a base class, which the
/// `gate:object` feature gate covers separately. And `traitWithPrivateMember.kt` needs a PRIVATE
/// computed property's accessor emitted private (accessor privacy isn't modeled — see the
/// `IfacePrivateComputed` rejection guard). Both must stay skipped (never a partial miscompile).
#[test]
fn corpus_gated_interface_property_shapes_stay_skipped() {
    if !common::corpus_ready() {
        return;
    }
    for case in [
        "traits/doubleDiamond.kt",
        "traits/traitWithPrivateMember.kt",
    ] {
        assert_eq!(
            common::run_box_corpus_case(case),
            None,
            "{case} needs an unmodeled feature — must stay skipped"
        );
    }
}
