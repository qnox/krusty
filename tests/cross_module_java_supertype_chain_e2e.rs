//! CROSS-MODULE Java-source supertype chains with an if/else-`null` member initializer: the
//! checked module's Java sources are provisioned as signature stubs on the classpath overlay (the
//! LSP analysis worker's wiring), while the DEPENDENCY module is already built — its Java classes
//! come from compiled `.class` files on the classpath (module build output), exactly as the LSP
//! provisions a dependency module whose build is current (`project::sources::covered_modules`).
//!
//! Real-world shape: intellij-community's
//! platform/platform-api/src/com/intellij/execution/ui/utils/FragmentsDslBuilder.kt L210 —
//! `private val validator = if (validation != null) ComponentValidator(this) else null` inside
//! `object : SettingsEditorFragment<Settings, Component>(…)`, where the anonymous object is
//! `Disposable` only through the cross-module chain SettingsEditorFragment (platform-api Java
//! SOURCE) → SettingsEditor (ide-core SOURCE, a DIFFERENT module) implements Disposable.
//!
//! Bisection showed the cross-module chain itself walks fine (conformance and member-resolution
//! tests below pass on their own); the false "cannot infer the type of property 'validator'" came
//! from the lightweight signature inferer, whose branch join (`common_lit_ty`) did not admit a
//! `null` branch — `if (c) x else null` typed as `Error` even though the full checker's `join`
//! types it `T?`. All shapes compile cleanly under kotlinc 2.4.10.

use super::common;
use krusty::jvm::classpath::Classpath;

/// Check `kotlin` with `stubs` provisioned as signature stubs on the classpath overlay and
/// `compiled` already built onto the classpath as loose `.class` files (a dependency module's
/// build output). Returns `None` when the JDK is unavailable (a SKIP, not a failure).
fn cross_module_diagnostics(
    kotlin: &str,
    stubs: &[&str],
    compiled: &[(&str, &str)],
) -> Option<Vec<String>> {
    let stdlib = common::stdlib_jar();
    let jdk = common::jdk_modules();
    let mut entries = vec![stdlib, jdk];
    if !compiled.is_empty() {
        let sources: Vec<(String, String)> = compiled
            .iter()
            .map(|(name, source)| (name.to_string(), source.to_string()))
            .collect();
        let (classes, _) = common::javac_compile(&sources, &[])?;
        entries.push(classes);
    }
    let cp = std::rc::Rc::new(Classpath::new(entries));
    cp.prepare_for_source_analysis();
    let sources: Vec<(String, String)> = stubs
        .iter()
        .map(|source| (String::new(), source.to_string()))
        .collect();
    let resolve = |cand: &str| cp.find_name(krusty::types::type_name(cand)).is_some();
    let stub_classes = krusty::jvm::java_stub::stub_classes(
        &sources,
        krusty::jvm::java_stub::StubMode::Lenient,
        &resolve,
    )
    .expect("stub generation");
    cp.set_stub_overlay(stub_classes);
    let platform = Box::new(krusty::jvm::jvm_libraries::JvmLibraries::new(cp));
    let inputs = [krusty::frontend::SourceInput::kotlin(kotlin)];
    let mut diags = krusty::diag::DiagSink::new();
    let _ = krusty::frontend::analyze_source_set_prefix_with_features(
        &inputs,
        1,
        1,
        platform,
        &krusty::features::LangFeatures::new(),
        &mut diags,
    );
    Some(diags.diags.iter().map(|d| d.msg.clone()).collect())
}

/// The dependency module (compiled): `Ed` is `AutoCloseable` only through the binary classpath.
const COMPILED_BASE: &[(&str, &str)] = &[(
    "Ed.java",
    r#"
public class Ed<S> implements AutoCloseable { public void close() {} }
"#,
)];

/// The checked module's Java sources (overlay stubs): the middle rung and the ctor consumer.
const STUB_FRAG: &str = r#"
public class Frag<S, C> extends Ed<S> {}
"#;
const STUB_TAKES: &str = r#"
public class Takes { public Takes(AutoCloseable d) {} }
"#;

/// `constructor_argument_conformance_walks_chain` (same file family, single-module stubs) with
/// the chain's base moved into a compiled dependency module: the walk must cross stub → binary.
#[test]
fn constructor_argument_conformance_crosses_compiled_module_boundary() {
    let kotlin = r#"
fun t() {
    val o = object : Frag<String, Any>() {}
    val f = Takes(o)
}
"#;
    let Some(d) = cross_module_diagnostics(kotlin, &[STUB_FRAG, STUB_TAKES], COMPILED_BASE)
    else {
        eprintln!("skipping: no javac/JDK");
        return;
    };
    assert_eq!(d, Vec::<String>::new());
}

/// `ComponentValidator(this)` inside the anonymous object's own property initializer — `this`
/// conforms to `Disposable` through the cross-module chain SettingsEditorFragment (stub) →
/// SettingsEditor (compiled, implements Disposable).
#[test]
fn anonymous_object_this_conforms_through_compiled_module_base() {
    let compiled: &[(&str, &str)] = &[
        ("Disposable.java", "public interface Disposable { void dispose(); }"),
        (
            "SettingsEditor.java",
            "public class SettingsEditor<S> implements Disposable { public void dispose() {} }",
        ),
    ];
    let frag = r#"
public class SettingsEditorFragment<S, C extends javax.swing.JComponent> extends SettingsEditor<S> {
    public C component() { return null; }
}
"#;
    let validator = r#"
public class ComponentValidator { public ComponentValidator(Disposable d) {} }
"#;
    let kotlin = r#"
import javax.swing.JComponent
class Settings
fun t() {
    val o = object : SettingsEditorFragment<Settings, JComponent>() {
        val validator = ComponentValidator(this)
    }
}
"#;
    let Some(d) = cross_module_diagnostics(kotlin, &[frag, validator], compiled) else {
        eprintln!("skipping: no javac/JDK");
        return;
    };
    assert_eq!(d, Vec::<String>::new());
}

/// The exact FragmentsDslBuilder.kt L210 shape: the anonymous object's type arguments are TYPE
/// PARAMETERS of the enclosing generic Kotlin class (`Settings : FragmentedSettings`,
/// `Component : JComponent`), the supertype constructor takes those type parameters through Java
/// wildcards/SAMs, and `validator`'s initializer is an if/else with a `null` branch around
/// `ComponentValidator(this)` — its type must infer as `ComponentValidator?`.
#[test]
fn anonymous_object_with_tparam_args_this_conforms_through_compiled_base() {
    let compiled: &[(&str, &str)] = &[
        ("Disposable.java", "public interface Disposable { void dispose(); }"),
        (
            "SettingsEditor.java",
            "public class SettingsEditor<S> implements Disposable { public void dispose() {} }",
        ),
    ];
    let fragmented = "public interface FragmentedSettings {}";
    let frag = r#"
public class SettingsEditorFragment<S, C extends javax.swing.JComponent> extends SettingsEditor<S> {
    public C component() { return null; }
    public SettingsEditorFragment(String id, C component,
                                  java.util.function.BiConsumer<? super S, ? super C> reset,
                                  java.util.function.Predicate<? super S> visible) {}
}
"#;
    let validator = r#"
public class ComponentValidator { public ComponentValidator(Disposable d) {} }
"#;
    let kotlin = r#"
import javax.swing.JComponent
class Fragment<Settings : FragmentedSettings, Component : JComponent>(
    val id: String,
    private val component: Component,
) {
    var validation: ((Settings, Component) -> String?)? = null
    var visible: (Settings) -> Boolean = { true }
    var reset: (Settings, Component) -> Unit = { _, _ -> }
    fun build(): SettingsEditorFragment<Settings, Component> {
        return object : SettingsEditorFragment<Settings, Component>(id, component, reset, visible) {
            private val validator = if (validation != null) ComponentValidator(this) else null
        }
    }
}
"#;
    let Some(d) = cross_module_diagnostics(kotlin, &[fragmented, frag, validator], compiled)
    else {
        eprintln!("skipping: no javac/JDK");
        return;
    };
    assert_eq!(d, Vec::<String>::new());
}

/// The FragmentsDslBuilder.kt L67 shape with the cross-module base: a three-rung stub chain
/// (NestedGroupFragment → SettingsEditorFragment → compiled SettingsEditor) — `component()`'s
/// declared `C` must still compose to `JComponent` so the inherited `isVisible` resolves.
#[test]
fn member_return_composes_chain_with_compiled_module_base() {
    let compiled: &[(&str, &str)] = &[
        ("Disposable.java", "public interface Disposable { void dispose(); }"),
        (
            "SettingsEditor.java",
            "public class SettingsEditor<S> implements Disposable { public void dispose() {} }",
        ),
    ];
    let frag = r#"
public class SettingsEditorFragment<S, C extends javax.swing.JComponent> extends SettingsEditor<S> {
    public C component() { return null; }
}
"#;
    let nested = r#"
public class NestedGroupFragment<S> extends SettingsEditorFragment<S, javax.swing.JComponent> {}
"#;
    let kotlin = r#"
class Settings
fun t() {
    val o = object : NestedGroupFragment<Settings>() {}
    val v: Boolean = o.component().isVisible
}
"#;
    let Some(d) = cross_module_diagnostics(kotlin, &[frag, nested], compiled) else {
        eprintln!("skipping: no javac/JDK");
        return;
    };
    assert_eq!(d, Vec::<String>::new());
}

/// The reduced trigger, independent of the module layout: an unannotated member property whose
/// initializer is `if (c) Call(this) else null` must infer the nullable branch join (`T?`), the
/// type the full checker's `join` gives the same expression.
#[test]
fn if_else_null_member_initializer_infers_nullable_join() {
    let kotlin = r#"
import javax.swing.JComponent
class Settings : FragmentedSettings
fun t(validation: ((Settings, JComponent) -> String?)?) {
    val o = object : SettingsEditorFragment<Settings, JComponent>() {
        private val validator = if (validation != null) ComponentValidator(this) else null
    }
}
"#;
    let compiled: &[(&str, &str)] = &[
        ("Disposable.java", "public interface Disposable { void dispose(); }"),
        (
            "SettingsEditor.java",
            "public class SettingsEditor<S> implements Disposable { public void dispose() {} }",
        ),
    ];
    let fragmented = "public interface FragmentedSettings {}";
    let frag = r#"
public class SettingsEditorFragment<S, C extends javax.swing.JComponent> extends SettingsEditor<S> {
    public C component() { return null; }
}
"#;
    let validator = r#"
public class ComponentValidator { public ComponentValidator(Disposable d) {} }
"#;
    let Some(d) = cross_module_diagnostics(kotlin, &[fragmented, frag, validator], compiled)
    else {
        eprintln!("skipping: no javac/JDK");
        return;
    };
    assert_eq!(d, Vec::<String>::new());
}

/// The same if/else-`null` initializer shape with no Java involved at all: the branch join fix is
/// in the platform-neutral signature inferer, not in any Java/classpath seam.
#[test]
fn if_else_null_member_initializer_infers_nullable_join_pure_kotlin() {
    let kotlin = r#"
open class Frag
class Takes(val d: Any)
fun t(c: Boolean) {
    val o = object : Frag() {
        private val validator = if (c) Takes(this) else null
    }
}
"#;
    let Some(d) = cross_module_diagnostics(kotlin, &[], &[]) else {
        eprintln!("skipping: no javac/JDK");
        return;
    };
    assert_eq!(d, Vec::<String>::new());
}

/// The real L67 shape — anon object with ctor args, `component().isVisible` called on the
/// implicit `this` inside a `?.let` lambda in a member function. PASSES: none of these
/// ingredients (ctor args, tparam type argument, safe-call lambda, implicit `this`) is what
/// breaks the real file's L67 — its remaining trigger is not reproduced here.
#[test]
fn member_return_composes_chain_with_ctor_args_in_safe_call_lambda() {
    let compiled: &[(&str, &str)] = &[
        ("Disposable.java", "public interface Disposable { void dispose(); }"),
        (
            "SettingsEditor.java",
            "public class SettingsEditor<S> implements Disposable { public void dispose() {} }",
        ),
    ];
    let fragmented = "public interface FragmentedSettings {}";
    let frag = r#"
public class SettingsEditorFragment<S, C extends javax.swing.JComponent> extends SettingsEditor<S> {
    public C component() { return null; }
    public SettingsEditorFragment(String id, C component,
                                  java.util.function.BiConsumer<? super S, ? super C> reset,
                                  java.util.function.Predicate<? super S> visible) {}
}
"#;
    let nested = r#"
public class NestedGroupFragment<S extends FragmentedSettings> extends SettingsEditorFragment<S, javax.swing.JComponent> {
    public NestedGroupFragment(String id, String name, String group,
                               java.util.function.Predicate<? super S> visible) {
        super(id, null, null, visible);
    }
}
"#;
    let kotlin = r#"
class Group<Settings : FragmentedSettings>(val id: String) {
    var applyVisibility: ((Settings, Boolean) -> Unit)? = null
    var name: String? = null
    var group: String? = null
    var visible: (Settings) -> Boolean = { true }
    fun build(): NestedGroupFragment<Settings> {
        return object : NestedGroupFragment<Settings>(id, name, group, visible) {
            fun applyEditorTo(s: Settings) {
                applyVisibility?.let { it(s, component().isVisible) }
            }
        }
    }
}
"#;
    let Some(d) = cross_module_diagnostics(kotlin, &[fragmented, frag, nested], compiled)
    else {
        eprintln!("skipping: no javac/JDK");
        return;
    };
    assert_eq!(d, Vec::<String>::new());
}
