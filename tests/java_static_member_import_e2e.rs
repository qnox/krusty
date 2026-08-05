//! Member imports of Java-declared STATIC members (`import p.PlatformDataKeys.TREE_EXPANDER`):
//! the bare imported-name rung in the checker only probed package-facade consts
//! (`top_level_static_field`, the `import kotlin.math.PI` shape) and never resolved the import
//! parent as a CLASSIFIER, so a `public static final` field on a Java class was `unresolved
//! reference` whether the class came from a Java-SOURCE stub overlay or the CLASSPATH. Found on
//! intellij-community's CollapseAllAction.kt / ExpandAllAction.kt. The fix resolves the import
//! parent with `nested_internal_name` and reads the member with `static_field_name` — one shared,
//! origin-blind rung mirroring the qualified `C.FIELD` read. All clean shapes compile under
//! kotlinc 2.4.10.

use super::common;
use krusty::jvm::classpath::Classpath;

/// Check `kotlin` against Java sources provisioned as signature stubs on the classpath overlay —
/// the exact wiring of the LSP analysis worker (`worker.rs::set_java_stub_overlay`). Returns the
/// checker's diagnostic messages for the checked Kotlin file.
fn java_stub_diagnostics(kotlin: &str, java: &[&str]) -> Vec<String> {
    java_stub_analysis(kotlin, java).0
}

/// As [`java_stub_diagnostics`], but also returns the analysis so a test can inspect the
/// checker-recorded resolution of a specific expression (const folding, enum-entry owner).
fn java_stub_analysis(
    kotlin: &str,
    java: &[&str],
) -> (Vec<String>, krusty::frontend::SourceSetAnalysis) {
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
    let analysis = krusty::frontend::analyze_source_set_prefix_with_features(
        &inputs,
        1,
        1,
        platform,
        &krusty::features::LangFeatures::new(),
        &mut diags,
    );
    let messages = diags.diags.iter().map(|d| d.msg.clone()).collect();
    (messages, analysis)
}

/// The `ExprId` of the sole `Expr::Name(name)` in the checked file — the read of the imported
/// member (imports are not expressions).
fn name_expr(analysis: &krusty::frontend::SourceSetAnalysis, name: &str) -> krusty::ast::ExprId {
    let file = &analysis.files[0];
    let mut found = file
        .expr_arena
        .iter()
        .enumerate()
        .filter(|(_, e)| matches!(e, krusty::ast::Expr::Name(n) if n == name));
    let (index, _) = found.next().expect("imported-member read expr");
    assert!(found.next().is_none(), "expected exactly one read of {name}");
    krusty::ast::ExprId(index as u32)
}

/// Check `kotlin` against the plain stdlib+JDK classpath (no stub overlay).
fn classpath_diagnostics(kotlin: &str) -> Vec<String> {
    classpath_analysis(kotlin).0
}

/// As [`classpath_diagnostics`], but also returns the analysis so a test can inspect the
/// checker-recorded resolution of a specific expression (const folding).
fn classpath_analysis(kotlin: &str) -> (Vec<String>, krusty::frontend::SourceSetAnalysis) {
    let stdlib = common::stdlib_jar();
    let jdk = common::jdk_modules();
    let cp = std::rc::Rc::new(Classpath::new(vec![stdlib, jdk]));
    cp.prepare_for_source_analysis();
    let platform = Box::new(krusty::jvm::jvm_libraries::JvmLibraries::new(cp));
    let inputs = [krusty::frontend::SourceInput::kotlin(kotlin)];
    let mut diags = krusty::diag::DiagSink::new();
    let analysis = krusty::frontend::analyze_source_set_prefix_with_features(
        &inputs,
        1,
        1,
        platform,
        &krusty::features::LangFeatures::new(),
        &mut diags,
    );
    let messages = diags.diags.iter().map(|d| d.msg.clone()).collect();
    (messages, analysis)
}

const PLATFORM_DATA_KEYS: &str = r#"
package p;
public class PlatformDataKeys {
    public static final String TREE_EXPANDER = "x";
    public static boolean isNewUI() { return true; }
}
"#;

/// The CollapseAllAction shape: a `public static final` field on a JAVA-SOURCE class imported by
/// member name reads cleanly.
#[test]
fn java_source_static_final_field_import_resolves() {
    let kotlin = r#"
import p.PlatformDataKeys.TREE_EXPANDER
fun f(): String = TREE_EXPANDER
"#;
    let d = java_stub_diagnostics(kotlin, &[PLATFORM_DATA_KEYS]);
    assert_eq!(d, Vec::<String>::new());
}

/// The gap is origin-agnostic: the same member import against a CLASSPATH Java class
/// (`java.math.BigInteger.ZERO`, a non-constant `public static final` field) resolves through the
/// same shared rung.
#[test]
fn classpath_static_final_field_import_resolves() {
    let kotlin = r#"
import java.math.BigInteger.ZERO
fun f(): java.math.BigInteger = ZERO
"#;
    let d = classpath_diagnostics(kotlin);
    assert_eq!(d, Vec::<String>::new());
}

/// Regression guard: a Java-source static METHOD imported by member name and CALLED keeps
/// resolving (methods already reached the companion-decode rung before this fix).
#[test]
fn java_source_static_method_import_and_call_resolves() {
    let kotlin = r#"
import p.PlatformDataKeys.isNewUI
fun f(): Boolean = isNewUI()
"#;
    let d = java_stub_diagnostics(kotlin, &[PLATFORM_DATA_KEYS]);
    assert_eq!(d, Vec::<String>::new());
}

/// Negative guard: importing a member the class does NOT declare still diagnoses in the
/// kotlinc-parity format, naming the missing member.
#[test]
fn java_source_import_of_nonexistent_member_reports_unresolved() {
    let kotlin = r#"
import p.PlatformDataKeys.NOPE
fun f(): String = NOPE
"#;
    let d = java_stub_diagnostics(kotlin, &[PLATFORM_DATA_KEYS]);
    assert!(
        d.iter().any(|m| m == "unresolved reference 'NOPE'."),
        "expected kotlinc-format unresolved-reference diag, got: {d:?}"
    );
}

/// CONST-FOLDING through the new rung: a `public static final int` with a compile-time-literal
/// initializer carries a `ConstantValue` attribute, so the imported-name read must record the
/// folded constant (the same inlining kotlinc performs at every use site) — not merely resolve.
/// Observed on the CLASSPATH origin (`java.lang.Integer.MAX_VALUE` on the real JDK class): the
/// Java-SOURCE stub overlay is signature-only (`java_stub` emits no `ConstantValue`), so there is
/// nothing to fold on that origin.
#[test]
fn classpath_static_final_int_import_folds_constant() {
    let kotlin = r#"
import java.lang.Integer.MAX_VALUE
fun f(): Int = MAX_VALUE
"#;
    let (d, analysis) = classpath_analysis(kotlin);
    assert_eq!(d, Vec::<String>::new());
    let e = name_expr(&analysis, "MAX_VALUE");
    let info = analysis.types[0].as_ref().expect("checked file types");
    let folded = info
        .resolved_library_companion_const(e)
        .expect("imported read must record the folded constant");
    assert_eq!(folded.ty, krusty::types::Ty::Int);
}

const OUTER: &str = r#"
package p;
public class Outer {
    public static class Inner {
        public static final String FIELD = "x";
    }
}
"#;

/// NESTED-CLASS import parent: the parent `p.Outer.Inner` only resolves through the
/// right-to-left `/`→`$` nested-class candidate walk in `nested_internal_name`.
#[test]
fn java_source_nested_class_static_field_import_resolves() {
    let kotlin = r#"
import p.Outer.Inner.FIELD
fun f(): String = FIELD
"#;
    let d = java_stub_diagnostics(kotlin, &[OUTER]);
    assert_eq!(d, Vec::<String>::new());
}

const COLOR: &str = r#"
package p;
public enum Color { RED, GREEN }
"#;

/// ENUM-ENTRY member import: the entry resolves as a static field and the read records the enum
/// owner so lowering emits the entry `getstatic`, exactly as a qualified `Color.RED` read.
#[test]
fn java_source_enum_entry_import_resolves() {
    let kotlin = r#"
import p.Color.RED
fun f(): p.Color = RED
"#;
    let (d, analysis) = java_stub_analysis(kotlin, &[COLOR]);
    assert_eq!(d, Vec::<String>::new());
    let e = name_expr(&analysis, "RED");
    let info = analysis.types[0].as_ref().expect("checked file types");
    let owner = info
        .resolved_enum_entry_owner(e)
        .expect("imported enum entry must record its owner");
    assert_eq!(owner.render(), "p/Color");
}
