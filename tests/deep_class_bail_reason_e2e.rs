//! Pins for the PRECISE `BackendOutcome::LowerBail` reason each pass-2 CLASS-region lowering guard
//! reports (companion to `lower_bail_reason_e2e.rs`, which pins the pre-pass-2 gates).
//!
//! Pass 2 sets the phase marker `deep:class` per class; every class-region guard that returned `None`
//! without refining it lumped 76 box-corpus skips into the survey's `lower: deep:class` blob. Each
//! guard now records its precise unsupported-feature boundary, so the survey buckets stay attributable
//! (a failure here means a guard lost — or never got — its label).
//!
//! Needs the provisioned box corpus + JVM toolchain; skips otherwise.

use super::common;

/// Single-file corpus cases pinned to their precise class-region guard reason (previously the
/// unrefined `deep:class`).
const GATED_CORPUS_CASES: &[(&str, &str)] = &[
    // `class Sub : Base(…)` super-constructor-call shapes the flat ctor emitter doesn't model.
    ("classes/inheritedInnerClass.kt", "gate:super-ctor-arity"),
    (
        "secondaryConstructors/callFromSubClass.kt",
        "gate:super-ctor-arity",
    ),
    (
        "classes/superConstructorCallWithComplexArg.kt",
        "gate:super-ctor-arg-mismatch",
    ),
    (
        "privateConstructors/withVarargs.kt",
        "gate:super-ctor-arg-mismatch",
    ),
    // Named/reordered super arguments (`: Base(k = …)`).
    (
        "secondaryConstructors/callFromPrimaryWithNamedArgs.kt",
        "gate:super-named-args",
    ),
    (
        "argumentOrder/argumentOrderInSuperCall.kt",
        "gate:super-named-args",
    ),
    // A suspend covariant override needs an erasure fixup the coroutine pass never applies.
    // (The corpus cases for the two suspend labels all import kotlinc's `helpers.*` test infra,
    // which the single-file helper doesn't inject — they are pinned inline below instead.)
    // `tailrec` on a member / companion method isn't loop-transformed (only top-level fns are).
    (
        "diagnostics/functions/tailRecursion/thisReferences.kt",
        "gate:tailrec-member",
    ),
    (
        "diagnostics/functions/tailRecursion/tailrecWithExplicitCompanionObjectDispatcher.kt",
        "gate:tailrec-companion",
    ),
    // Enum guards: an interface member the enum doesn't visibly satisfy; an entry-body method that
    // isn't an override of an enum/interface member.
    (
        "specialBuiltins/enumAsOrdinaled.kt",
        "gate:enum-unsatisfied-interface-member",
    ),
    ("enum/kt18731_2.kt", "gate:enum-entry-non-override"),
    // An anonymous object over a parameterized base could capture the enclosing instance (KT-3684).
    (
        "argumentOrder/argumentOrderInObjectSuperCall.kt",
        "gate:anon-object-outer-capture",
    ),
    ("classes/selfcreate.kt", "gate:anon-object-outer-capture"),
    // A subclass overrides a property a base member reads internally (base reads bypass virtual
    // dispatch).
    (
        "properties/kt1168.kt",
        "gate:base-reads-override-internally",
    ),
    (
        "bridges/substitutionInSuperClass/property.kt",
        "gate:base-reads-override-internally",
    ),
    // A branchy body-property initializer (`when`/`if`/`try`/…) in the flat ctor emitter.
    ("lazyCodegen/when.kt", "gate:branchy-field-initializer"),
    ("regressions/kt3587.kt", "gate:branchy-field-initializer"),
    // A delegated `var` whose `setValue` is an EXTENSION fun (accessor synth only looks up members).
    (
        "delegatedProperty/setAsExtensionFun.kt",
        "gate:delegate-extension-setvalue",
    ),
    // An override param/return typed by a class type-param with a class bound (bound-aware erasure).
    (
        "defaultArguments/function/covariantOverrideGeneric.kt",
        "gate:bound-type-param-override",
    ),
];

#[test]
fn gated_corpus_cases_report_precise_class_lower_bail() {
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

/// Backend outcome for an inline source (the same checked-file → JVM-backend pipeline as the CLI),
/// or `None` when the toolchain is absent / the front end rejects the source.
fn outcome(src: &str) -> Option<common::BackendOutcome> {
    let jdk = common::jdk_modules()?;
    let cp = krusty::toolchain::classpath_jars_for(src);
    common::backend_outcome_in_process(src, "P", &cp, Some(&jdk))
}

fn assert_lower_bail(src: &str, reason: &str) {
    if !common::stdlib_toolchain_ready() {
        return;
    }
    assert_eq!(
        outcome(src),
        Some(common::BackendOutcome::LowerBail(reason.to_string())),
        "source must stop at its precise unsupported lowering boundary:\n{src}"
    );
}

#[test]
fn suspend_covariant_override_reports_erasure_gate() {
    // A suspend member overriding a (non-generic) interface method with a value-class COVARIANT
    // return needs an erasure bridge the coroutine pass never fixes up.
    assert_lower_bail(
        r#"
@JvmInline
value class IC(val s: String)

interface IBar {
    suspend fun bar(): Any
}

class Test : IBar {
    override suspend fun bar(): IC = IC("OK")
}

fun box(): String = "OK"
"#,
        "gate:suspend-override-erasure",
    );
}

#[test]
fn suspend_lambda_in_class_member_reports_gate() {
    // A suspend lambda expression inside a class member isn't modeled (the synthesized lambda class
    // shape assumes a top-level owner).
    assert_lower_bail(
        r#"
class C {
    fun g(): suspend () -> Int = suspend { 1 }
}

fun box(): String = "OK"
"#,
        "gate:suspend-lambda-in-class",
    );
}
