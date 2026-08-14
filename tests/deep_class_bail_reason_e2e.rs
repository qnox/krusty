//! Pins for the PRECISE `BackendOutcome::LowerBail` reason each pass-2 CLASS-region lowering guard
//! reports (companion to `lower_bail_reason_e2e.rs`, which pins the pre-pass-2 gates).
//!
//! Pass 2 sets the phase marker `deep:class` per class; every class-region guard that returned `None`
//! without refining it lumped 76 box-corpus skips into the survey's `lower: deep:class` blob. Each
//! guard now records its precise unsupported-feature boundary, so the survey buckets stay attributable
//! (a failure here means a guard lost — or never got — its label).
//!
use super::common;

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
