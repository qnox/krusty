//! BACKEND-REJECTION coverage: valid Kotlin that the front end accepts but the IR *backend* cleanly
//! DECLINES to lower, emitting a "not yet supported by the IR backend" style diagnostic (a non-zero
//! exit). The box corpus contains only SUPPORTED programs, so these bail branches
//! (`src/jvm/backend.rs`, `src/ir_lower.rs`, `src/jvm/suspend.rs`, `src/jvm/value_classes.rs`) are
//! otherwise never exercised. Each test drives the same front-end + JVM backend pipeline in-process
//! (front end passes; the backend bails) and asserts the compile is rejected.
//!
//! These are deliberately constructs krusty does NOT support yet — if one of them starts compiling,
//! the feature has landed and the test should be promoted to a real round-trip test elsewhere.

use super::common;

/// Compile `src` through the frontend and JVM backend in-process. Returns `true` only when the front
/// end accepts the source and the backend reaches one of its unsupported-feature exits. Returns
/// `true` (skip-clean) when the toolchain is absent, so the suite never fails spuriously on a machine
/// without the vendored kotlinc/JDK.
fn rejects(src: &str) -> bool {
    let Some(stdlib) = common::stdlib_jar() else {
        return true;
    };
    let Some(jdk_modules) = common::jdk_modules() else {
        return true;
    };
    common::backend_rejects_in_process(src, "S", &[stdlib], Some(&jdk_modules)).unwrap_or(false)
}

// --- Unsigned byte/short block-list (src/jvm/ir_emit.rs `ty_ok`/`callee_ok`; emit_all → None; the
//     backend surfaces "this construct is not yet supported by the IR backend"). The corpus never
//     uses `UByte`/`UShort` in a lowered function, so these bail branches are otherwise unreached. ---

#[test]
fn ubyte_conversion_rejected() {
    // `200.toUByte()` returns a `UByte`, a block-listed stdlib value class.
    assert!(rejects(
        "fun main() { val x = 200.toUByte(); println(x) }\n"
    ));
}

#[test]
fn ubyte_literal_conversion_rejected() {
    assert!(rejects("fun main() { println((1).toUByte()) }\n"));
}

#[test]
fn ubyte_parameter_rejected() {
    // A `UByte` parameter type puts the block-listed type into the method descriptor.
    assert!(rejects(
        "fun f(x: UByte): UByte = x\nfun main() { println(f(1.toUByte())) }\n"
    ));
}

#[test]
fn ushort_return_rejected() {
    assert!(rejects(
        "fun g(): UShort = 1.toUShort()\nfun main() { println(g()) }\n"
    ));
}

#[test]
fn ubyte_to_int_rejected() {
    assert!(rejects(
        "fun main() { val b = 5.toUByte(); println(b.toInt()) }\n"
    ));
}

// --- Mixed spread in a vararg call (`f(0, *a, 3)`) — SpreadBuilder path not modeled; src/ir_lower.rs
//     bails ("call f"), backend surfaces the generic unsupported diagnostic. ---

#[test]
fn mixed_spread_vararg_rejected() {
    assert!(rejects(
        "fun f(vararg xs: Int) = xs.sum()\nfun main() { val a = intArrayOf(1, 2); println(f(0, *a, 3)) }\n"
    ));
}

// --- Delegated properties (`by`) — not lowered yet; src/ir_lower.rs deep-bails. Several distinct
//     delegate providers (custom operator, `lazy`, a `Map`) all take the same bail path. ---

#[test]
fn delegated_property_observable_rejected() {
    assert!(rejects(
        "import kotlin.properties.Delegates\n\
         class C { var x: Int by Delegates.observable(0) { _, _, _ -> } }\n\
         fun main() { val c = C(); c.x = 5; println(c.x) }\n"
    ));
}

#[test]
fn delegated_property_lazy_accepted() {
    // `by lazy` now resolves its `getValue` through the classpath extension seam (LazyKt) — accepted.
    assert!(!rejects(
        "class C { val x: Int by lazy { 5 } }\nfun main() { println(C().x) }\n"
    ));
}

#[test]
fn delegated_property_map_accepted() {
    // `by map` resolves `Map.getValue` (a classpath extension in MapsKt) — accepted.
    assert!(!rejects(
        "class C(m: Map<String, Any?>) { val name: String by m }\n\
         fun main() { println(C(mapOf(\"name\" to \"a\")).name) }\n"
    ));
}

// --- Suspend-function shapes the state-machine builder declines (src/jvm/suspend.rs → lower_suspend
//     returns false; backend surfaces "this suspend-function shape is not yet supported"). Each shape
//     exercises a distinct un-handled control-flow construct around a suspension point. ---

#[test]
fn suspend_try_finally_rejected() {
    // A NON-suspending finally around a suspending try body IS supported (see
    // suspend_try_finally_body_e2e). A SUSPENDING finally would itself span coroutine states — still
    // unmodeled, so it must bail rather than miscompile.
    assert!(rejects(
        "suspend fun d() {}\n\
         suspend fun f() { try { d() } finally { d() }; d() }\n"
    ));
}

#[test]
fn suspend_try_catch_rejected() {
    assert!(rejects(
        "suspend fun d(): Int = 1\n\
         suspend fun f(): Int { try { return d() } catch (e: Exception) { return d() } }\n"
    ));
}

#[test]
fn suspend_return_in_try_rejected() {
    assert!(rejects(
        "suspend fun d(): Int = 1\n\
         suspend fun f(): Int { try { return d() } finally {} }\n"
    ));
}

#[test]
fn suspend_try_as_expression_rejected() {
    assert!(rejects(
        "suspend fun d(): Int = 1\n\
         suspend fun f(): Int { val x = try { d() } catch (e: Exception) { 0 }; return x }\n"
    ));
}

// NOTE: a suspend call in a compound-assignment inside a `while`/`do-while`/`for` loop
// (`while (s < 3) { s += d() }`) is now LOWERED (the coroutine pass hoists the suspension to a temp).
// Promoted to a round-trip test in `suspend_loop_compound_assign_e2e.rs`.

#[test]
fn suspend_when_with_multiple_suspensions_rejected() {
    assert!(rejects(
        "suspend fun d(): Int = 1\n\
         suspend fun f(x: Int): Int = when (x) { 0 -> d(); else -> d() + d() }\n"
    ));
}

// A suspend lambda body on a LAZY `Sequence.map` (returns `Sequence`, not `List`) must NOT be inlined
// into the `List`-materializing accumulate-loop desugar (that would hand back an `ArrayList` where the
// static type is `Sequence` → VerifyError). The `List`-result + `kotlin/collections` facade guard
// excludes it, so it falls through to the `FunctionN` path and the backend cleanly DECLINES. (kotlinc
// also rejects this outright — "suspension functions can only be called within coroutine body".)
#[test]
fn suspend_sequence_map_not_inlined_rejected() {
    assert!(rejects(
        "interface R { suspend fun g(x: Int): Int }\n\
         suspend fun f(s: Sequence<Int>, r: R): Sequence<Int> = s.map { r.g(it) }\n"
    ));
}

#[test]
fn suspend_safe_call_double_suspension_rejected() {
    assert!(rejects(
        "class Box { suspend fun d(): Int = 1 }\n\
         suspend fun f(b: Box?): Int { return (b?.d() ?: 0) + (b?.d() ?: 0) }\n"
    ));
}
