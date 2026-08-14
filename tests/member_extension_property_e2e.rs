//! MEMBER EXTENSION PROPERTIES in classes (`class C { val Int.x: T get() = … }`).
//!
//! kotlinc emits the accessor exactly like a member extension FUNCTION named `getX`/`setX`: an
//! instance method on `C` whose JVM param 0 is the extension receiver (`getX(I)L…;`), called with
//! the dispatch receiver as `this`. krusty already lowers member extension functions; this wires
//! the same machinery for properties: pass 1 registers the accessor, pass 2 lowers its body with
//! `$dispatch` at slot 0 and extension `this` at slot 1, and the checker records the selected
//! owner/receiver at each read/write site (`ExprLowering::MemberExtensionPropertyRead`,
//! `StmtLowering::MemberExtensionPropertyWrite`).
//!
//! Deliberately still gated (never miscompile): a receiver or return mentioning a class type
//! parameter's OWN type (`val T.x: T`), a value-class receiver/return (mangling/boxing), property
//! type parameters (`val <T> T.x`), delegated member extension properties, and
//! open/override/abstract extension properties (cross-class extension dispatch isn't modeled).

use super::common;

fn run_box(src: &str, stem: &str) {
    let Some(out) = common::compile_and_run_with_stdlib(src, stem) else {
        panic!("{stem}: expected the box to compile and run");
    };
    assert_eq!(out, "OK", "{stem}");
}

/// The basic shape: a `val` member extension property with an expression getter, read through a
/// qualified receiver inside the declaring class.
#[test]
fn member_ext_prop_basic() {
    run_box(
        r#"
class Test {
    val Int.foo: String
        get() = "OK"

    fun test(): String {
        return 1.foo
    }
}

fun box(): String {
    return Test().test()
}
"#,
        "MemberExtPropBasic",
    );
}

/// A block-body getter.
#[test]
fn member_ext_prop_block_getter() {
    run_box(
        r#"
class Test {
    val Int.foo: String
        get() {
            return "OK"
        }

    fun test(): String {
        return 1.foo
    }
}

fun box(): String {
    return Test().test()
}
"#,
        "MemberExtPropBlockGetter",
    );
}

/// A PRIVATE getter: callable inside the class, emitted non-virtual (`invokespecial`).
#[test]
fn member_ext_prop_private_getter() {
    run_box(
        r#"
class Test {
    private val Long.baz: Int
        get() = 42

    fun test(): String {
        val l = 1L
        return if (l.baz == 42) "OK" else "Fail"
    }
}

fun box(): String {
    return Test().test()
}
"#,
        "MemberExtPropPrivateGetter",
    );
}

/// A `var` member extension property with custom get+set: the setter writes through the dispatch
/// receiver, the getter reads it back.
#[test]
fn member_ext_prop_var_custom_setter() {
    run_box(
        r#"
class Test {
    var storage = "Fail"

    var Int.foo: String
        get() = storage
        set(value) {
            storage = value
        }

    fun test(): String {
        val i = 1
        i.foo = "OK"
        return i.foo
    }
}

fun box(): String {
    return Test().test()
}
"#,
        "MemberExtPropVarSetter",
    );
}

/// A `private set` accessor: writable inside the class, and the setter is emitted non-virtual.
#[test]
fn member_ext_prop_private_setter() {
    run_box(
        r#"
class Test {
    var storage = "Fail"

    var Int.foo: String
        get() = storage
        private set(str: String) {
            storage = str
        }

    fun test(): String {
        val i = 1
        i.foo = "OK"
        return i.foo
    }
}

fun box(): String {
    return Test().test()
}
"#,
        "MemberExtPropPrivateSetter",
    );
}

/// A `private set` remains private when the dispatch receiver is supplied implicitly outside the
/// declaring class. The member-extension resolver must carry accessor visibility independently
/// from the public property; otherwise the checker accepts the write and lowering attempts an
/// illegal call to the private JVM method.
#[test]
fn member_ext_prop_private_setter_is_inaccessible_outside_the_owner() {
    let jdk = common::jdk_modules();
    let source = r#"
class SyntheticOwner {
    var Int.token: String
        get() = "readable"
        private set(value) {}
}

fun box(): String = with(SyntheticOwner()) {
    1.token = "not allowed"
    1.token
}
"#;
    let classpath = krusty::toolchain::classpath_jars_for(source);
    let outcome = common::backend_outcome_in_process(
        source,
        "MemberExtPropPrivateSetterAccess",
        &classpath,
        Some(jdk.as_path()),
    );
    assert_ne!(
        outcome,
        Some(common::BackendOutcome::Emitted),
        "a public member extension property must not expose its private setter"
    );
}

/// Primitive receivers of different widths (`Double` vs `Long`): the receiver parameter keeps its
/// own slot shape, and two same-named-on-different-receivers properties coexist on one class.
#[test]
fn member_ext_prop_primitive_receivers() {
    run_box(
        r#"
class Test {
    var doubleStorage = "fail"
    var longStorage = "fail"

    var Double.foo: String
        get() = doubleStorage
        set(value) {
            doubleStorage = value
        }

    var Long.bar: String
        get() = longStorage
        set(value) {
            longStorage = value
        }

    fun test(): String {
        val d = 1.0
        d.foo = "O"
        val l = 1L
        l.bar = "K"
        return d.foo + l.bar
    }
}

fun box(): String {
    return Test().test()
}
"#,
        "MemberExtPropPrimitives",
    );
}

/// `this@<propName>` inside the accessor denotes the EXTENSION receiver (slot 1), not dispatch.
#[test]
fn member_ext_prop_labeled_this() {
    run_box(
        r#"
class Test {
    val Int.innerGetter: Int
        get() {
            return this@innerGetter
        }

    fun test(): Int {
        val i = 1
        if (i.innerGetter != 1) return 0
        return 1
    }
}

fun box(): String {
    if (Test().test() != 1) return "inner getter or setter failed"
    return "OK"
}
"#,
        "MemberExtPropLabeledThis",
    );
}

/// The dispatch receiver from an implicit lambda receiver (`with(Test()) { … }`), not lexical
/// `this`.
#[test]
fn member_ext_prop_with_lambda_dispatch() {
    run_box(
        r#"
class Test {
    val Int.foo: String
        get() = "OK"
}

fun box(): String {
    with(Test()) {
        return 1.foo
    }
}
"#,
        "MemberExtPropWithDispatch",
    );
}

/// Receiver OVERLOADS: two same-named member extension properties on different receivers —
/// `getX(I)` and `getX(D)` are distinct JVM methods, each linked its own body.
#[test]
fn member_ext_prop_receiver_overloads() {
    run_box(
        r#"
class Test {
    val Int.x: String
        get() = "O"
    val Double.x: String
        get() = "K"

    fun test(): String {
        return 1.x + 1.0.x
    }
}

fun box(): String {
    return Test().test()
}
"#,
        "MemberExtPropOverloads",
    );
}

/// A plain computed property and a member extension property of the SAME name: `getX()` and
/// `getX(I)` coexist, and each accessor gets its own body.
#[test]
fn member_ext_prop_name_collision_with_computed() {
    run_box(
        r#"
class Test {
    val x: String
        get() = "O"
    val Int.x: String
        get() = "K"

    fun test(): String {
        return x + 1.x
    }
}

fun box(): String {
    return Test().test()
}
"#,
        "MemberExtPropNameCollision",
    );
}

/// A member extension property whose accessor name collides with an INHERITED generic method: the
/// accessor is a FRESH declaration, so the backend must not derive a bridge over `Base.getX(T)` —
/// a call through the base reference keeps the base implementation.
#[test]
fn member_ext_prop_no_bridge_over_inherited_generic() {
    run_box(
        r#"
open class Base {
    fun <T> getX(t: T): String = "B"
}

class C : Base() {
    val Int.x: String
        get() = "OK"

    fun test(): String = 1.x
}

fun box(): String {
    val c = C()
    if (c.test() != "OK") return "f1"
    val b: Base = c
    if (b.getX("q") != "B") return "f2"
    return "OK"
}
"#,
        "MemberExtPropNoBridge",
    );
}

/// A compound assignment (`i.foo += v`) lowers through the read+write handoff with the receiver
/// evaluated once.
#[test]
fn member_ext_prop_compound_assign() {
    run_box(
        r#"
class Test {
    var storage = "Fail"
    var receiverCalls = 0

    var Int.foo: String
        get() = storage
        set(value) {
            storage = value
        }

    fun nextReceiver(): Int {
        receiverCalls += 1
        return 1
    }

    fun test(): String {
        storage = "O"
        nextReceiver().foo += "K"
        return if (receiverCalls == 1) 1.foo else "receiver evaluated twice"
    }
}

fun box(): String {
    return Test().test()
}
"#,
        "MemberExtPropCompound",
    );
}

/// The exact corpus cases.
#[test]
fn corpus_member_ext_props_box_ok() {
    if !common::corpus_ready() {
        return;
    }
    for case in [
        "extensionProperties/inClass.kt",
        "extensionProperties/inClassWithGetter.kt",
        "extensionProperties/inClassWithPrivateGetter.kt",
        "extensionProperties/inClassWithSetter.kt",
        "extensionProperties/inClassWithPrivateSetter.kt",
        "extensionProperties/inClassLongTypeInReceiver.kt",
        "labels/propertyInClassAccessor.kt",
    ] {
        assert_eq!(
            common::run_box_corpus_case(case).as_deref(),
            Some("OK"),
            "{case} must execute successfully, not silently skip"
        );
    }
}

/// The corpus generic-receiver case (`class Test<T> { val T.foo }`) needs the erased/rebound
/// receiver handoff — gated in pass 1. Must stay skipped (never a partial miscompile).
#[test]
fn corpus_generic_receiver_member_ext_prop_stays_skipped() {
    if !common::corpus_ready() {
        return;
    }
    assert_eq!(
        common::run_box_corpus_case("extensionProperties/extensionMemberWithTypeParameter.kt"),
        None,
        "extensionMemberWithTypeParameter needs the generic-receiver tier — must stay skipped"
    );
}

/// REJECTION GUARDS: shapes that must never EMIT (the accessor/dispatch isn't modeled). Asserts
/// on the backend outcome, not a run result — a skip and an emitted-but-crashing class both make a
/// run-based check pass, but only the former is acceptable.
#[test]
fn unsupported_member_ext_prop_shapes_still_rejected() {
    let jdk = common::jdk_modules();
    let cases: &[(&str, &str)] = &[
        // A DELEGATED member extension property (`by Del()`): the delegate's getValue receives a
        // KProperty + the extension receiver — a splice this path doesn't build.
        (
            "MemberExtPropDelegated",
            r#"
import kotlin.reflect.KProperty

class Del {
    operator fun getValue(t: Int, p: KProperty<*>): String = "OK"
}

class Test {
    val Int.foo: String by Del()
    fun test(): String = 1.foo
}

fun box(): String = Test().test()
"#,
        ),
        // A property TYPE PARAMETER (`val <T> T.x`): its erasure/bound handling isn't modeled here.
        (
            "MemberExtPropGenericProp",
            r#"
class Test {
    val <T> T.foo: String
        get() = "OK"

    fun test(): String = 1.foo
}

fun box(): String = Test().test()
"#,
        ),
        // An OPEN member extension property: cross-class extension overrides register nothing.
        (
            "MemberExtPropOpen",
            r#"
open class Base {
    open val Int.foo: String
        get() = "Fail"
}

class Derived : Base() {
    override val Int.foo: String
        get() = "OK"
}

fun box(): String = with(Derived()) { 1.foo }
"#,
        ),
        // A source method can spell the exact accessor JVM signature. Registering both would emit
        // duplicate `getToken(I)Ljava/lang/String;` methods; the class must be rejected instead of
        // relying on the JVM to fail while loading it.
        (
            "MemberExtPropAccessorCollision",
            r#"
class SyntheticOwner {
    val Int.token: String
        get() = "property"

    fun getToken(value: Int): String = "method"
}

fun box(): String = with(SyntheticOwner()) { 1.token }
"#,
        ),
        // Nullability is metadata, not part of the JVM parameter descriptor. The collision guard
        // must compare target-provided physical descriptors, so `String?` does not evade a
        // `getToken(String)` declaration that owns the same bytecode signature.
        (
            "MemberExtPropNullableAccessorCollision",
            r#"
class SyntheticOwner {
    val String?.token: String
        get() = "property"

    fun getToken(value: String): String = "method"
}

fun box(): String = with(SyntheticOwner()) { null.token }
"#,
        ),
        // Setter accessors obey the same physical-signature rule as getters.
        (
            "MemberExtPropSetterCollision",
            r#"
class SyntheticOwner {
    var Int.token: String
        get() = "property"
        set(value) {}

    fun setToken(receiver: Int, value: String) {}
}

fun box(): String = with(SyntheticOwner()) { 1.token }
"#,
        ),
    ];
    for (stem, src) in cases {
        let cp = krusty::toolchain::classpath_jars_for(src);
        let outcome = common::backend_outcome_in_process(src, stem, &cp, Some(jdk.as_path()));
        assert_ne!(
            outcome,
            Some(common::BackendOutcome::Emitted),
            "{stem}: unsupported member extension property shape must not emit (skip, never miscompile)"
        );
    }
}

#[test]
fn class_type_parameter_member_extension_property_runs() {
    run_box(
        r#"
class Test<T>(val v: T) {
    val T.foo: T
        get() = v

    fun test(t: T): T = t.foo
}

fun box(): String = Test("OK").test("x")
"#,
        "MemberExtPropTParamRet",
    );
}
