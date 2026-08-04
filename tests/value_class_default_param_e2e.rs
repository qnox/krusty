//! Default parameter values that CONSTRUCT a value class (`fun test(z: Z = Z(42))`), routed through
//! the emitted `test-<hash>$default` stub.
//!
//! `toplevel_default_stub_safe` rejected ANY value-class `IrExpr::New` in a default expression —
//! the erased stub "doesn't box/unbox". But the value-class pass rewrites `new Z(…)` →
//! `constructor-impl(…)` in registered defaults exactly as in function bodies, so the stub can
//! re-emit the erased construction directly. Sound only behind the existing signature carve-outs
//! (non-generic value class, non-nullable non-nested-VC underlying); anything else keeps the
//! conservative skip.

use super::common;

fn run_box(src: &str, stem: &str) {
    let Some(out) = common::compile_and_run_with_stdlib(src, stem) else {
        panic!("{stem}: expected the box to compile and run");
    };
    assert_eq!(out, "OK", "{stem}");
}

/// A value class whose UNDERLYING has a default (`Vid()` → the stub re-emits
/// `constructor-impl$default(0, 1, null)`), and a Long underlying (placeholder width).
#[test]
fn value_class_default_underlying_variants() {
    run_box(
        r#"
@JvmInline
value class Vid(val id: Int = 7)

fun test(v: Vid = Vid()) = v.id

fun box(): String {
    if (test() != 7) return "f1"
    if (test(Vid(9)) != 9) return "f2"
    return "OK"
}
"#,
        "VcDefaultUnderlyingDefault",
    );
    run_box(
        r#"
@JvmInline
value class L(val x: Long)

fun test(l: L = L(42L)) = l.x

fun box(): String {
    if (test() != 42L) return "f1"
    if (test(L(123L)) != 123L) return "f2"
    return "OK"
}
"#,
        "VcDefaultLong",
    );
}

/// The corpus shape: a value-class default on a top-level function, omitted at the call site.
#[test]
fn value_class_default_via_stub() {
    run_box(
        r#"
@JvmInline
value class Z(val z: Int)

fun test(z: Z = Z(42)) = z.z

fun box(): String {
    if (test() != 42) return "f1"
    if (test(Z(123)) != 123) return "f2"
    return "OK"
}
"#,
        "VcDefaultSimple",
    );
}

/// Mixed slots: the value-class default plus a plain default, partial omission.
#[test]
fn value_class_default_mixed_slots() {
    run_box(
        r#"
@JvmInline
value class Z(val z: Int)

fun test(a: Int = 1, z: Z = Z(2), b: String = "3") = "$a${z.z}$b"

fun box(): String {
    if (test() != "123") return "f1:${test()}"
    if (test(9) != "923") return "f2"
    if (test(b = "K") != "12K") return "f3"
    if (test(7, Z(8), "9") != "789") return "f4"
    return "OK"
}
"#,
        "VcDefaultMixed",
    );
}

/// The exact corpus case.
#[test]
fn corpus_value_class_defaults_box_ok() {
    if !common::corpus_ready() {
        return;
    }
    assert_eq!(
        common::run_box_corpus_case(
            "inlineClasses/defaultParameterValues/defaultParameterValuesOfInlineClassType.kt"
        )
        .as_deref(),
        Some("OK"),
        "corpus case must execute successfully, not silently skip"
    );
}

/// The Boxing corpus case needs a BOXED slot fill (`Z?`/`Any`/interface param with a value-class
/// default) — carved out; it must stay skipped (promote when boxed stub slots land).
#[test]
fn corpus_value_class_default_boxing_stays_skipped() {
    if !common::corpus_ready() {
        return;
    }
    assert_eq!(
        common::run_box_corpus_case(
            "inlineClasses/defaultParameterValues/defaultParameterValuesOfInlineClassTypeBoxing.kt"
        ),
        None,
        "boxed value-class default slots are not modeled — must stay skipped"
    );
}

/// A NULLABLE value-class param over a REFERENCE underlying with a non-construction default —
/// the erased slot is a plain reference (`String`), so the stub is sound and must keep compiling.
#[test]
fn nullable_vc_param_reference_underlying() {
    run_box(
        r#"
@JvmInline
value class S(val x: String)

fun computeS(): S? = S("OK")

fun foo(s: S? = computeS()) = s?.x ?: "f"

fun box(): String {
    if (foo() != "OK") return "f1"
    if (foo(null) != "f") return "f2"
    if (foo(S("K")) != "K") return "f3"
    return "OK"
}
"#,
        "VcDefaultNullableRefUnderlying",
    );
}

/// Explicit-argument calls on BOXED-slot shapes: no stub is emitted (the root-slot gate rejects),
/// but the file must still compile and run.
#[test]
fn boxed_slot_shapes_explicit_args_run() {
    run_box(
        r#"
@JvmInline
value class Z(val z: Int)

fun testN(z: Z? = Z(42)) = z!!.z
fun testA(z: Any = Z(42)) = (z as Z).z

fun box(): String {
    if (testN(Z(1)) != 1) return "f1"
    if (testA(Z(2)) != 2) return "f2"
    return "OK"
}
"#,
        "VcDefaultBoxedExplicit",
    );
}

/// A value-class default read from a LOCAL parameter (physically erased) stays sound.
#[test]
fn value_class_default_from_param() {
    run_box(
        r#"
@JvmInline
value class Z(val z: Int)

fun test(z2: Z, z: Z = z2) = z.z

fun box(): String {
    if (test(Z(5)) != 5) return "f1"
    if (test(Z(5), Z(9)) != 9) return "f2"
    return "OK"
}
"#,
        "VcDefaultFromParam",
    );
}

/// REJECTION GUARDS: a value-class-typed default read from a BOXED source (a field or getter —
/// krusty keeps those boxed, the erased slot doesn't) must not emit a stub; the omitted call
/// stays a skip, never a class-load VerifyError.
#[test]
fn boxed_read_defaults_still_rejected() {
    let jdk = common::jdk_modules();
    let cases: &[(&str, &str)] = &[
        (
            "VcDefaultTopLevelValRead",
            r#"
@JvmInline
value class Z(val z: Int)

val gz: Z = Z(7)

fun test(z: Z = gz) = z.z

fun box(): String = test().toString()
"#,
        ),
        (
            "VcDefaultObjectGetterRead",
            r#"
@JvmInline
value class Z(val z: Int)

object O {
    val z: Z = Z(7)
}

fun test(z: Z = O.z) = z.z

fun box(): String = test().toString()
"#,
        ),
    ];
    for (stem, src) in cases {
        let cp = krusty::toolchain::classpath_jars_for(src);
        let outcome = common::backend_outcome_in_process(src, stem, &cp, Some(jdk.as_path()));
        assert_ne!(
            outcome,
            Some(common::BackendOutcome::Emitted),
            "{stem}: boxed-read value-class default must not emit (skip, never miscompile)"
        );
    }
}

/// REJECTION GUARDS: the carve-outs must keep these from EMITTING — not merely skipping at
/// runtime. Asserting `!= Emitted` (not `is_none()`) because a miscompile here produces classes
/// that fail verification at class load, which a run-based `is_none()` cannot distinguish from a
/// sound skip.
#[test]
fn unsafe_value_class_defaults_still_rejected() {
    let jdk = common::jdk_modules();
    let cases: &[(&str, &str)] = &[
        (
            "VcDefaultGeneric",
            r#"
@JvmInline
value class G<T>(val s: T)

fun test(g: G<String> = G("OK")) = g.s

fun box(): String = test()
"#,
        ),
        (
            "VcDefaultNullableUnderlying",
            r#"
@JvmInline
value class N(val s: String?)

fun test(n: N = N("OK")) = n.s ?: "f"

fun box(): String = test()
"#,
        ),
        // A NULLABLE value-class parameter: its stub slot stays boxed — the erased default
        // construction doesn't fill it.
        (
            "VcDefaultNullableParam",
            r#"
@JvmInline
value class Z(val z: Int)

fun test(z: Z? = Z(42)) = z!!.z

fun box(): String = test().toString()
"#,
        ),
        // An `Any`/interface slot: the default must be box-impl'd into it — not modeled.
        (
            "VcDefaultAnySlot",
            r#"
@JvmInline
value class Z(val z: Int)

fun test(z: Any = Z(42)) = (z as Z).z.toString()

fun box(): String = test()
"#,
        ),
        // A NESTED value-class underlying erases through extra layers the stub doesn't cover.
        (
            "VcDefaultNestedUnderlying",
            r#"
@JvmInline
value class I(val x: Int)
@JvmInline
value class O(val i: I)

fun test(o: O = O(I(42))) = o.i.x

fun box(): String = test().toString()
"#,
        ),
    ];
    for (stem, src) in cases {
        let cp = krusty::toolchain::classpath_jars_for(src);
        let outcome = common::backend_outcome_in_process(src, stem, &cp, Some(jdk.as_path()));
        assert_ne!(
            outcome,
            Some(common::BackendOutcome::Emitted),
            "{stem}: unsafe value-class default must not emit (skip, never miscompile)"
        );
    }
}
