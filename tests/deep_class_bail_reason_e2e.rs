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
    // A delegated `var` whose `setValue` is not resolved as a member. This corpus case uses an
    // extension, but the diagnostic stays provider-agnostic because missing member metadata is the
    // only fact the lowering guard observes.
    (
        "delegatedProperty/setAsExtensionFun.kt",
        "gate:delegate-setvalue-unresolved",
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

#[test]
fn suspend_covariant_override_reports_erasure_gate() {
    // A suspend member overriding a (non-generic) interface method with a value-class COVARIANT
    // return needs an erasure bridge the coroutine pass never fixes up. The gate lives in the bridge
    // pass (bridges are derived in the JVM backend), so the file stops there rather than in lowering.
    common::assert_inline_source_backend_bail(
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
        krusty::jvm::backend::SkipReason::Bridges,
    );
}

#[test]
fn suspend_lambda_in_class_member_reports_gate() {
    // A suspend lambda expression inside a class member isn't modeled (the synthesized lambda class
    // shape assumes a top-level owner).
    common::assert_inline_source_lower_bail(
        r#"
class C {
    fun g(): suspend () -> Int = suspend { 1 }
}

fun box(): String = "OK"
"#,
        "gate:suspend-lambda-in-class",
    );
}

#[test]
fn enum_external_supertype_reports_unavailable_obligations() {
    // The lowering guard has no abstract-obligation inventory for a supertype outside this AST. The
    // fixture uses a platform interface, but the reason is intentionally identical for another source
    // file or module: provenance is not the unsupported semantic fact.
    common::assert_inline_source_lower_bail(
        r#"
enum class ExternalEnum : java.util.function.Supplier<String> {
    ONLY;
    override fun get(): String = "OK"
}

fun box(): String = "OK"
"#,
        "gate:enum-supertype-obligations-unavailable",
    );
}

#[test]
fn generic_enum_entry_override_reports_erasure_gate() {
    // Per-entry implementations of a generic interface need bridges on each synthesized entry
    // subclass. The enum-level bridge path cannot stand in for those subclass-local bridges.
    common::assert_inline_source_lower_bail(
        r#"
interface GenericAction<T> {
    fun apply(value: T): String
}

enum class EntryEnum : GenericAction<String> {
    ONLY {
        override fun apply(value: String): String = value
    }
}

fun box(): String = "OK"
"#,
        "gate:enum-entry-override-erasure",
    );
}

#[test]
fn enum_entry_custom_property_reports_shape_gate() {
    // Entry-subclass property emission models a plain initialized backing field only. A computed
    // getter has a different class shape and must remain a deliberate bail rather than inheriting the
    // coarse class-phase marker.
    common::assert_inline_source_lower_bail(
        r#"
enum class PropertyEnum {
    ONLY {
        val value: Int get() = 1
    }
}

fun box(): String = "OK"
"#,
        "gate:enum-entry-property-shape",
    );
}

#[test]
fn user_class_name_cannot_impersonate_an_anonymous_object() {
    if !common::stdlib_toolchain_ready() {
        return;
    }
    // This ordinary declaration intentionally resembles the parser's current synthetic-name format.
    // Anonymous-object policy must follow the AST ownership map, never generated or user-written text;
    // the old substring check incorrectly applied the outer-capture gate to this valid class.
    const SRC: &str = r#"
open class Base(val value: Int)

class `Regular$anon$Name` : Base(1) {
    fun result(): Int = value
}

fun box(): String = if (`Regular$anon$Name`().result() == 1) "OK" else "fail"
"#;
    assert_eq!(
        common::inline_source_backend_outcome(SRC),
        Some(common::BackendOutcome::Emitted),
        "a user class name must not activate anonymous-object lowering policy"
    );
}
