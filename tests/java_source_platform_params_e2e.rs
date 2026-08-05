//! Java-SOURCE-declared member parameters are PLATFORM types (flexible `String!`), exactly like
//! Java-CLASSPATH members already are: a nullable argument is accepted, a non-nullable argument
//! is accepted, and an incompatible argument still diagnoses in kotlinc's format. The Java files
//! reach the checker through the same signature-stub classpath overlay the LSP analysis worker
//! provisions (`jvm::java_stub` → `Classpath::set_stub_overlay`). Found on intellij-community's
//! HelpTooltip.kt (`this.toolTipText = html?.toString()`) and FragmentsDslBuilder.kt
//! (`it.actionHint = actionHint`). All clean shapes compile under kotlinc 2.4.10.

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

const W: &str = r#"
public class W {
    public void setToolTipText(String s) {}
    public String getToolTipText() { return null; }
    public void take(String s) {}
}
"#;

/// A synthetic Java-source setter parameter is platform `String!`: a nullable argument assigns
/// cleanly.
#[test]
fn java_source_setter_accepts_nullable_argument() {
    let kotlin = r#"
fun f(w: W, s: String?) {
    w.toolTipText = s
}
"#;
    let d = java_stub_diagnostics(kotlin, &[W]);
    assert_eq!(d, Vec::<String>::new());
}

/// A Java-source method parameter is platform `String!`: a nullable call argument binds cleanly.
#[test]
fn java_source_method_accepts_nullable_argument() {
    let kotlin = r#"
fun f(w: W, s: String?) {
    w.take(s)
}
"#;
    let d = java_stub_diagnostics(kotlin, &[W]);
    assert_eq!(d, Vec::<String>::new());
}

/// Non-nullable arguments keep working on both shapes.
#[test]
fn java_source_params_accept_non_nullable_arguments() {
    let kotlin = r#"
fun f(w: W, s: String) {
    w.toolTipText = s
    w.take(s)
}
"#;
    let d = java_stub_diagnostics(kotlin, &[W]);
    assert_eq!(d, Vec::<String>::new());
}

/// Platform nullability does not weaken the base type: an `Int` argument to the `String!` setter
/// parameter still reports the assignment-mismatch diagnostic naming the DECLARED parameter type
/// (the value is not nullable, so no widening applies) — identical to the classpath-Java form.
#[test]
fn java_source_setter_rejects_incompatible_argument() {
    let kotlin = r#"
fun f(w: W, i: Int) {
    w.toolTipText = i
}
"#;
    let d = java_stub_diagnostics(kotlin, &[W]);
    assert_eq!(
        d,
        vec!["assignment type mismatch: actual type is 'Int', but 'String' was expected.".to_string()]
    );
}

/// Same guard on the method-call shape: an `Int` argument to the `String!` parameter diagnoses —
/// the same candidate-rejection message the classpath-Java form of this call reports.
#[test]
fn java_source_method_rejects_incompatible_argument() {
    let kotlin = r#"
fun f(w: W, i: Int) {
    w.take(i)
}
"#;
    let d = java_stub_diagnostics(kotlin, &[W]);
    assert_eq!(
        d,
        vec!["none of the following candidates is applicable:\n\nfun take(_: String): Unit".to_string()]
    );
}

/// The platform RETURN side: a Java-source getter's `String!` result binds to both `String` and
/// `String?` declarations.
#[test]
fn java_source_getter_return_binds_to_both_nullabilities() {
    let kotlin = r#"
fun f(w: W) {
    val a: String = w.toolTipText
    val b: String? = w.toolTipText
}
"#;
    let d = java_stub_diagnostics(kotlin, &[W]);
    assert_eq!(d, Vec::<String>::new());
}

/// Parity anchor: the same flexible-nullability rule already governed Java-CLASSPATH members
/// (`javax.swing.JLabel.setText` from the JDK), and the fix keeps both origins on one mechanism —
/// a nullable argument to a classpath Java synthetic setter assigns cleanly.
#[test]
fn classpath_java_setter_accepts_nullable_argument() {
    let stdlib = common::stdlib_jar();
    let jdk = common::jdk_modules();
    let cp = std::rc::Rc::new(Classpath::new(vec![stdlib, jdk]));
    cp.prepare_for_source_analysis();
    let platform = Box::new(krusty::jvm::jvm_libraries::JvmLibraries::new(cp));
    let kotlin = r#"
fun f(l: javax.swing.JLabel, s: String?) {
    l.text = s
    l.setText(s)
}
"#;
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
    let d: Vec<String> = diags.diags.iter().map(|d| d.msg.clone()).collect();
    assert_eq!(d, Vec::<String>::new());
}

/// An UNQUALIFIED write (`toolTipText = s`) with the Java-source class as the IMPLICIT receiver
/// (`with(w) { … }`) resolves the setter through the implicit-receiver walk, whose assignment
/// check applies the same platform widening as the explicit-member path.
#[test]
fn java_source_implicit_receiver_setter_accepts_nullable_argument() {
    let kotlin = r#"
fun f(w: W, s: String?) {
    with(w) {
        toolTipText = s
    }
}
"#;
    let d = java_stub_diagnostics(kotlin, &[W]);
    assert_eq!(d, Vec::<String>::new());
}

/// The explicit-`this` form of the Kotlin-subclass shape: the write walks from the source class
/// into the Java-source supertype's setter (`resolve_external_inherited_property_setter`), and
/// the inherited platform parameter still accepts the nullable value. (The UNQUALIFIED form of
/// this shape — `toolTipText = s` inside `class K : W()` — currently reports
/// `unresolved reference`: the implicit-receiver walk does not consult the external-inherited
/// fallback at all, a pre-existing resolution gap independent of nullability.)
#[test]
fn java_source_subclass_this_setter_accepts_nullable_argument() {
    let kotlin = r#"
class K : W() {
    fun g(s: String?) {
        this.toolTipText = s
    }
}
"#;
    let d = java_stub_diagnostics(kotlin, &[W]);
    assert_eq!(d, Vec::<String>::new());
}

const TOGGLE: &str = r#"
public class Toggle {
    public void setEnabled(boolean b) {}
    public boolean getEnabled() { return false; }
}
"#;

/// A PRIMITIVE setter parameter is never a platform type: the `is_reference` guard keeps
/// `setEnabled(boolean)` exact, so a nullable `Boolean?` value still diagnoses — byte-identical
/// to the classpath-Java form of this mismatch.
#[test]
fn java_source_primitive_setter_rejects_nullable_argument() {
    let kotlin = r#"
fun f(t: Toggle, b: Boolean?) {
    t.enabled = b
}
"#;
    let d = java_stub_diagnostics(kotlin, &[TOGGLE]);
    assert_eq!(
        d,
        vec!["assignment type mismatch: actual type is 'Boolean?', but 'Boolean' was expected.".to_string()]
    );
}

const BASE: &str = r#"
public class Base {
    public void setActionHint(String s) {}
    public String getActionHint() { return null; }
}
"#;

const DERIVED: &str = r#"
public class Derived extends Base {}
"#;

/// The FragmentsDslBuilder shape: the setter is declared on a Java-source SUPERTYPE of the
/// receiver's type, so resolution walks the stub overlay's supertype chain — and the inherited
/// setter's parameter is still platform `String!`.
#[test]
fn java_source_inherited_setter_accepts_nullable_argument() {
    let kotlin = r#"
fun f(d: Derived, s: String?) {
    d.actionHint = s
}
"#;
    let d = java_stub_diagnostics(kotlin, &[BASE, DERIVED]);
    assert_eq!(d, Vec::<String>::new());
}
