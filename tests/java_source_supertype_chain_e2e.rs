//! Java-SOURCE supertype chains: a Kotlin anonymous object extends a Java-source generic class
//! whose own supertype is ANOTHER Java-source generic class. Member resolution and conformance
//! through that chain must apply the COMPOSED substitution — at `Mid`, `S := String`; at `Base`,
//! `C := JComponent` — so an inherited member's declared type variable resolves to its actual
//! argument and members of that argument (or supertypes of the chain) are visible.
//!
//! The Java files reach the checker exactly as the LSP analysis worker provisions them: parsed
//! into signature stubs (`jvm::java_stub`) installed as a classpath overlay
//! (`Classpath::set_stub_overlay`), then the checked Kotlin file runs through
//! `analyze_source_set_prefix_with_features_trimmed`. All shapes compile cleanly under kotlinc
//! 2.4.10. Found on intellij-community's
//! platform/platform-api/src/com/intellij/execution/ui/utils/FragmentsDslBuilder.kt
//! (`component().isVisible`, `it.actionHint = …`, `ComponentValidator(this)`), where
//! NestedGroupFragment.java extends SettingsEditorFragment.java extends SettingsEditor.

use super::common;
use krusty::jvm::classpath::Classpath;

/// Check `kotlin` against Java sources provisioned as signature stubs on the classpath overlay —
/// the exact wiring of the LSP analysis worker (`worker.rs::set_java_stub_overlay`). Returns the
/// checker's diagnostic messages for the checked Kotlin file.
fn java_stub_diagnostics(kotlin: &str, java: &[&str]) -> Vec<String> {
    let stdlib = common::stdlib_jar();
    let jdk = common::jdk_modules();
    let cp = std::rc::Rc::new(Classpath::new(vec![stdlib, jdk]));
    cp.prepare_for_source_analysis();
    let sources: Vec<(String, String)> = java
        .iter()
        .map(|source| (String::new(), source.to_string()))
        .collect();
    let resolve = |cand: &str| cp.find_name(krusty::types::type_name(cand)).is_some();
    let stubs = krusty::jvm::java_stub::stub_classes(
        &sources,
        krusty::jvm::java_stub::StubMode::Lenient,
        &resolve,
    )
    .expect("stub generation");
    cp.set_stub_overlay(stubs);
    let platform = Box::new(krusty::jvm::jvm_libraries::JvmLibraries::new(cp));
    let inputs = [krusty::frontend::SourceInput::kotlin(kotlin)];
    let mut diags = krusty::diag::DiagSink::new();
    let _ = krusty::frontend::analyze_source_set_prefix_with_features_trimmed(
        &inputs,
        1,
        1,
        platform,
        &krusty::features::LangFeatures::new(),
        &mut diags,
    );
    diags.diags.iter().map(|d| d.msg.clone()).collect()
}

const BASE: &str = r#"
public class Base<S, C extends javax.swing.JComponent> {
    public C component() { return null; }
    public String getActionHint() { return null; }
    public void setActionHint(String h) {}
}
"#;

const MID: &str = r#"
public class Mid<S> extends Base<S, javax.swing.JComponent> {}
"#;

/// `component()`'s declared return is the type variable `C`; through the anonymous object's
/// chain (`Mid<String>` → `Base<String, JComponent>`) it reads as `JComponent`, whose inherited
/// `isVisible()` synthetic property then resolves.
#[test]
fn anonymous_object_member_return_composes_chain_substitution() {
    let kotlin = r#"
fun t() {
    val o = object : Mid<String>() {}
    val v: Boolean = o.component().isVisible
}
"#;
    let d = java_stub_diagnostics(kotlin, &[BASE, MID]);
    assert_eq!(d, Vec::<String>::new());
}

/// The same chain with a plain (named) receiver type instead of an anonymous object, isolating
/// the composed substitution from the anonymous-object supertype walk.
#[test]
fn applied_receiver_member_return_composes_chain_substitution() {
    let kotlin = r#"
fun t(m: Mid<String>) {
    val v: Boolean = m.component().isVisible
}
"#;
    let d = java_stub_diagnostics(kotlin, &[BASE, MID]);
    assert_eq!(d, Vec::<String>::new());
}

/// A synthetic SETTER on an inherited Java member: `actionHint` comes from
/// `getActionHint`/`setActionHint` declared on `Base`, reached through the anonymous object's
/// `Mid` supertype.
#[test]
fn anonymous_object_synthetic_setter_through_chain() {
    let kotlin = r#"
fun t() {
    val o = object : Mid<String>() {}
    o.actionHint = "x"
}
"#;
    let d = java_stub_diagnostics(kotlin, &[BASE, MID]);
    assert_eq!(d, Vec::<String>::new());
}

/// The substituted return must be exact, not just non-`Any`: assigning the `JComponent` result
/// to an incompatible type stays an error.
#[test]
fn composed_substituted_return_is_exact() {
    let kotlin = r#"
fun t() {
    val o = object : Mid<String>() {}
    val wrong: String = o.component()
}
"#;
    let d = java_stub_diagnostics(kotlin, &[BASE, MID]);
    assert_eq!(d.len(), 1, "expected exactly one diagnostic, got: {d:?}");
    assert!(
        d[0].contains("type mismatch") || d[0].contains("initializer type mismatch"),
        "expected a type-mismatch diagnostic, got: {d:?}"
    );
}

/// A getter-only inherited synthetic property is a `val`: writing it through the chain reports
/// kotlinc's read-only diagnostic, not `unresolved reference`.
#[test]
fn anonymous_object_getter_only_synthetic_property_is_val() {
    let base = r#"
public class Base<S, C extends javax.swing.JComponent> {
    public C component() { return null; }
    public String getOnly() { return null; }
}
"#;
    let kotlin = r#"
fun t() {
    val o = object : Mid<String>() {}
    o.only = "x"
}
"#;
    let d = java_stub_diagnostics(kotlin, &[base, MID]);
    assert_eq!(d, vec!["'val' cannot be reassigned.".to_string()]);
}

/// Conformance through the chain: the anonymous object is an `AutoCloseable` only because
/// `Frag` extends `Ed` which implements it — a Java-source supertype chain walk during argument
/// checking.
#[test]
fn constructor_argument_conformance_walks_chain() {
    let ed = r#"
public class Ed<S> implements AutoCloseable { public void close() {} }
"#;
    let frag = r#"
public class Frag<S, C> extends Ed<S> {}
"#;
    let takes = r#"
public class Takes { public Takes(AutoCloseable d) {} }
"#;
    let kotlin = r#"
fun t() {
    val o = object : Frag<String, Any>() {}
    val f = Takes(o)
}
"#;
    let d = java_stub_diagnostics(kotlin, &[ed, frag, takes]);
    assert_eq!(d, Vec::<String>::new());
}

