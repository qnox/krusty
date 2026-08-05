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

// The intellij-community shape: a sealed class whose companion holds a `@JvmField val` (plus a
// `@JvmStatic fun`, which already resolved) in a dependency module.
const SEALED_DEP: &str = r#"
package dep

sealed class AnActionResult {
    class Failed : AnActionResult()
    companion object {
        @JvmField val PERFORMED: AnActionResult = object : AnActionResult() {}
        @JvmStatic fun failed(): AnActionResult = Failed()
    }
}
"#;

#[test]
fn jvm_field_companion_val_of_sealed_class_resolves() {
    let d = fallback_diagnostics(
        r#"
package app

import dep.AnActionResult

fun performed(): AnActionResult = AnActionResult.PERFORMED
fun failed(): AnActionResult = AnActionResult.failed()
"#,
        SEALED_DEP,
    );
    assert_resolves(&d, "AnActionResult.PERFORMED / AnActionResult.failed()");
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

// The ActionUiKind shape: an interface whose companion holds ONLY `@JvmField val`s.
#[test]
fn interface_companion_jvm_field_vals_resolve() {
    let d = fallback_diagnostics(
        r#"
package app

import dep.ActionUiKind

fun compact(): ActionUiKind = ActionUiKind.COMPACT
fun full(): ActionUiKind = ActionUiKind.FULL
"#,
        r#"
package dep

interface ActionUiKind {
    companion object {
        @JvmField val COMPACT: ActionUiKind = object : ActionUiKind {}
        @JvmField val FULL: ActionUiKind = object : ActionUiKind {}
    }
}
"#,
    );
    assert_resolves(&d, "ActionUiKind.COMPACT / ActionUiKind.FULL");
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
    assert!(
        d.iter()
            .any(|m| m.contains("unresolved reference 'Limits'.")),
        "expected the absent-member diagnostic to stay unchanged, got: {d:?}"
    );
}

// An `internal` companion property is module-scoped: the dependent module's read must keep
// reporting the same unresolved-reference diagnostic (kotlinc rejects cross-module internal
// access), exactly like the `public_functions`/`public_properties` filters hide internal
// callables.
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
    assert!(
        d.iter()
            .any(|m| m.contains("unresolved reference 'Limits'.")),
        "expected cross-module internal access to stay rejected, got: {d:?}"
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
