//! Pins for the PRECISE `BackendOutcome::LowerBail` reason each pre-pass-2 lowering gate reports.
//!
//! `src/ir_lower.rs` seeds the caller-owned bail sink with the catch-all `"deep"` and is supposed to
//! refine it as lowering progresses — but every gate/pass-1 bail that returned `None` without a
//! `set_bail` left the reason unrefined, lumping ~195 box-corpus skips into one unactionable survey
//! bucket (`lower: deep`, the top skip-bucket). Each gate now records its precise unsupported-feature
//! boundary so the survey stays attributable; a failure here means a bail path lost (or never got) its
//! label.
//!
//! Needs the provisioned box corpus + JVM toolchain; skips otherwise.

use super::common;

/// Single-file corpus cases pinned to their precise lowering-gate reason (previously the unrefined
/// catch-all `deep`).
const GATED_CORPUS_CASES: &[(&str, &str)] = &[
    // `sequence { yieldAll(…) }` — the yield-builder suspend state machine isn't modeled.
    ("coroutines/kt35967.kt", "gate:yield-builder"),
    // A suspend member on a class with a GENERIC supertype needs an erasure bridge the coroutine
    // lowering doesn't synthesize (`SuspendingMutableMap<K, V> : Map<K, V>`).
    (
        "coroutines/bridges/mapSuspendClear.kt",
        "gate:suspend-erasure-bridge",
    ),
    // An extension `suspend fun` isn't modeled.
    (
        "coroutines/inlineClasses/direct/createOverride.kt",
        "gate:extension-suspend-fn",
    ),
    // Member delegated-property shapes the inline accessor doesn't model: a value-class delegate,
    // and an EXTENSION `getValue` (no member `getValue` on the delegate class).
    (
        "inlineClasses/propertyDelegation/kt27070.kt",
        "gate:member-delegate-shape",
    ),
    (
        "delegatedProperty/getAsExtensionFun.kt",
        "gate:member-delegate-shape",
    ),
    // A top-level EXTENSION delegated property (`val Recv.x by …`) isn't modeled.
    (
        "extensionProperties/extensionPropertyDelegated.kt",
        "gate:extension-delegated-property",
    ),
    (
        "delegatedProperty/delegateForExtProperty.kt",
        "gate:extension-delegated-property",
    ),
    // Interface-delegation forwarder synthesis declines an intersection-typed delegate while
    // registering the class (pass 1a).
    (
        "delegation/delegationToIntersectionType3.kt",
        "gate:delegation-forwarders",
    ),
];

#[test]
fn gated_corpus_cases_report_precise_lower_bail() {
    if !common::corpus_ready() {
        return;
    }
    for &(case, reason) in GATED_CORPUS_CASES {
        assert_eq!(
            common::box_corpus_case_backend_outcome(case),
            Some(common::BackendOutcome::LowerBail(reason.to_string())),
            "{case} must stop at its precise unsupported lowering boundary"
        );
    }
}

#[test]
fn companion_with_explicit_base_args_reports_companion_synth() {
    // `companion object : Base(args)` with explicit base args isn't modeled in the synthesized
    // `C$Companion` registration — a pass-1a bail that must keep its phase label.
    common::assert_inline_source_lower_bail(
        r#"
open class Base(val x: Int)
class C {
    companion object : Base(1)
}
fun box(): String = "OK"
"#,
        "deep:companion-synth",
    );
}

#[test]
fn non_suspend_body_referencing_suspend_fn_reports_gate() {
    // A NON-suspend body that textually uses a suspend fn's name: call-site continuation threading
    // is only modeled inside a suspend body. The gate is a conservative TEXTUAL scan — here `sum`
    // is a local variable shadowing the suspend fn, a false positive the gate still declines on
    // (sound: it skips rather than risks a miscompile).
    common::assert_inline_source_lower_bail(
        r#"
suspend fun sum(x: Int): Int = x
fun box(): String {
    var sum = 1
    sum += 1
    return if (sum == 2) "OK" else "fail"
}
"#,
        "gate:suspend-call-from-non-suspend",
    );
}

#[test]
fn member_delegate_with_provide_delegate_reports_gate() {
    // A member property whose delegate declares `provideDelegate` isn't modeled by the inline
    // accessor.
    common::assert_inline_source_lower_bail(
        r#"
import kotlin.reflect.KProperty

class Delegate(val v: String) {
    operator fun getValue(thisRef: Any?, property: KProperty<*>): String = v
    operator fun provideDelegate(thisRef: Any?, property: KProperty<*>): Delegate = this
}

class C {
    val x: String by Delegate("OK")
}

fun box(): String = C().x
"#,
        "gate:member-delegate-shape",
    );
}

#[test]
fn unsupported_call_bucket_uses_ast_shape_not_source_name() {
    // The expression fallback used to publish the concrete callee (`call suspendCoroutine`). Keep
    // only its generic AST shape so local, module, and classpath call failures share one category and
    // neither source names nor generated JVM owners escape into the survey.
    common::assert_inline_source_lower_bail(
        r#"
import kotlin.coroutines.suspendCoroutine

suspend fun pause(): Unit = suspendCoroutine { }
fun box(): String = "OK"
"#,
        "call Name",
    );
}
