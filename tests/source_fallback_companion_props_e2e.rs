//! Dependency-source fallback: companion PROPERTIES of a workspace-source class in a dependency
//! module must resolve through the source-fallback path (`SourceFallbackPlatform`) exactly like the
//! companion FUNCTIONS and nested classifiers that already do.
//!
//! The dependent file is checked while the dependency file is only parsed + signature-collected —
//! the provisioning the LSP gives a workspace dependency — so these go through the same frontend
//! entry point (`analyze_source_set_prefix_with_features`) instead of an LSP round trip.
//!
//! Known limitations of the fallback projection (each is a separate future fix):
//! - a companion `var` WRITE through the fallback does not resolve (the projection serves the
//!   static field READ only; the jar path serves writes through the companion's setter);
//! - a `const val` resolves but does not const-fold (the source signature does not record the
//!   constant value the jar path serves through `companion_consts`).

use super::common;
use krusty::jvm::classpath::Classpath;

/// Check `checked` against a dependency module whose Kotlin source is provisioned through the
/// source-fallback path. Returns the checker's diagnostic messages for the checked file.
fn fallback_diagnostics(checked: &str, dependency: &str) -> Vec<String> {
    let stdlib = common::stdlib_jar();
    let jdk = common::jdk_modules();
    let cp = std::rc::Rc::new(Classpath::new(vec![stdlib, jdk]));
    let platform = Box::new(krusty::jvm::jvm_libraries::JvmLibraries::new(cp));
    let inputs = [
        krusty::frontend::SourceInput::kotlin(checked),
        krusty::frontend::SourceInput::kotlin(dependency),
    ];
    let mut diags = krusty::diag::DiagSink::new();
    let _ = krusty::frontend::analyze_source_set_prefix_with_features(
        &inputs,
        1,
        1,
        platform,
        &krusty::features::LangFeatures::new(),
        &mut diags,
    );
    diags.diags.iter().map(|d| d.msg.clone()).collect()
}

fn assert_resolves(d: &[String], what: &str) {
    let unresolved: Vec<_> = d
        .iter()
        .filter(|m| m.contains("unresolved reference"))
        .collect();
    assert!(
        unresolved.is_empty(),
        "expected {what} to resolve, got: {d:?}"
    );
}

// A sealed dependency type whose companion holds a `@JvmField val` plus a `@JvmStatic fun`. The
// function is the control: it already crossed the fallback boundary before property projection.
const SEALED_STATUS_DEP: &str = r#"
package dep

sealed class StatusResult {
    class Failure : StatusResult()
    companion object {
        @JvmField val COMPLETED: StatusResult = object : StatusResult() {}
        @JvmStatic fun failure(): StatusResult = Failure()
    }
}
"#;

#[test]
fn jvm_field_companion_val_of_sealed_class_resolves() {
    let d = fallback_diagnostics(
        r#"
package app

import dep.StatusResult

fun completed(): StatusResult = StatusResult.COMPLETED
fun failure(): StatusResult = StatusResult.failure()
"#,
        SEALED_STATUS_DEP,
    );
    assert_resolves(&d, "StatusResult.COMPLETED / StatusResult.failure()");
}

#[test]
fn plain_companion_val_resolves() {
    let d = fallback_diagnostics(
        r#"
package app

import dep.Widget

fun title(): String = Widget.title
"#,
        r#"
package dep

class Widget {
    companion object {
        val title: String = "w"
    }
}
"#,
    );
    assert_resolves(&d, "Widget.title");
}

#[test]
fn companion_const_val_resolves() {
    let d = fallback_diagnostics(
        r#"
package app

import dep.Limits

fun max(): Int = Limits.MAX
"#,
        r#"
package dep

class Limits {
    companion object {
        const val MAX: Int = 42
    }
}
"#,
    );
    assert_resolves(&d, "Limits.MAX");
}

// An interface whose companion holds only `@JvmField val`s ensures the projection does not depend
// on a sibling companion function having caused the companion classifier to be materialized first.
#[test]
fn interface_companion_jvm_field_vals_resolve() {
    let d = fallback_diagnostics(
        r#"
package app

import dep.PresentationMode

fun compact(): PresentationMode = PresentationMode.COMPACT
fun full(): PresentationMode = PresentationMode.FULL
"#,
        r#"
package dep

interface PresentationMode {
    companion object {
        @JvmField val COMPACT: PresentationMode = object : PresentationMode {}
        @JvmField val FULL: PresentationMode = object : PresentationMode {}
    }
}
"#,
    );
    assert_resolves(&d, "PresentationMode.COMPACT / PresentationMode.FULL");
}

#[test]
fn source_enum_entry_keeps_using_the_shared_field_realization() {
    // Enum entries already crossed this fallback as static classifier values. Companion fields now
    // use the same semantic field projection with a deferred physical token, so pin the pre-existing
    // enum path as a control: unifying provider-neutral projection must not trade one source-backed
    // field shape for another.
    let d = fallback_diagnostics(
        r#"
package app

import dep.Signal

fun ready(): Signal = Signal.READY
"#,
        r#"
package dep

enum class Signal { READY, WAITING }
"#,
    );
    assert_resolves(&d, "Signal.READY");
}

#[test]
fn computed_companion_property_resolves_from_its_declaration() {
    // A computed property is still a source declaration. Resolution selects that declaration; the
    // eventual dependency artifact supplies its accessor realization, and no field is invented.
    let d = fallback_diagnostics(
        r#"
package app

import dep.Settings

fun dynamic(): String = Settings.dynamic
"#,
        r#"
package dep

class Settings {
    companion object {
        val dynamic: String get() = "computed"
    }
}
"#,
    );
    assert_resolves(&d, "Settings.dynamic");
}

// A genuinely-absent member must keep reporting the same unresolved-reference diagnostic.
#[test]
fn absent_companion_member_still_reports_unresolved() {
    let d = fallback_diagnostics(
        r#"
package app

import dep.Limits

fun max(): Int = Limits.MISSING
"#,
        r#"
package dep

class Limits {
    companion object {
        const val MAX: Int = 42
    }
}
"#,
    );
    assert_eq!(d, ["unresolved reference 'MISSING'."]);
}

// An `internal` companion property remains a declaration across the dependency boundary, but it is
// inaccessible there. Kotlinc reports accessibility rather than pretending the declaration is absent.
#[test]
fn internal_companion_val_is_hidden_cross_module() {
    let d = fallback_diagnostics(
        r#"
package app

import dep.Limits

fun hidden(): Int = Limits.HIDDEN
"#,
        r#"
package dep

class Limits {
    companion object {
        internal val HIDDEN: Int = 7
    }
}
"#,
    );
    assert_eq!(
        d,
        ["cannot access 'HIDDEN': it is internal in 'dep/Limits$Companion'"]
    );
}

// The same `internal` companion property read from its OWN module keeps working.
#[test]
fn internal_companion_val_resolves_within_module() {
    let d = common::front_end_diagnostics(
        r#"
package dep

class Limits {
    companion object {
        internal val HIDDEN: Int = 7
    }
}

fun hidden(): Int = Limits.HIDDEN
"#,
        &[],
        None,
    );
    assert_resolves(&d, "in-module Limits.HIDDEN");
}
