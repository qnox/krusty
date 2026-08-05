//! Dependency-source fallback: a member PROPERTY of a GENERIC class in a dependency module must
//! resolve with the receiver's type arguments substituted — `ExtensionPointName<Customizer>`'s
//! `extensionList: List<T>` reads as `List<Customizer>`, not the erased `List<Any>`.
//!
//! The dependent file is checked while the dependency file is only parsed + signature-collected
//! (the provisioning the LSP gives a workspace dependency — see
//! `source_fallback_companion_props_e2e.rs` for the same fallback path). The fallback's
//! declaration walk returned the property's DECLARED type with the class type parameter unbound,
//! so the element type collapsed to `Any` and member calls on it — a `forEach` lambda parameter or
//! a `for`-loop variable — reported `unresolved reference`. The same chain against an inferred
//! (same-analysis) or classpath-jar provider substituted fine.
//!
//! Found on intellij-community's
//! platform/platform-api/src/com/intellij/execution/process/GeneralCommandLineEnvCustomizerService.kt:12
//! (`CommandLineEnvCustomizer.EP_NAME.extensionList.forEach { customizer -> … }`) and
//! platform/platform-api/src/com/intellij/ide/FocusedComponentProvider.kt:19
//! (`for (provider in EP_NAME.extensionList)`); both compile cleanly under kotlinc.

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

/// The dependency: a generic class with a `List<T>` member property (backing-field form).
const EPN_FIELD: &str = r#"
package extensions
class ExtensionPointName<T : Any>(name: String) {
    val extensionList: List<T> = emptyList()
}
"#;

/// The same dependency with a computed getter instead of a backing field (the real
/// ExtensionPointName.kt shape: `get() = getRootPoint().extensionList`).
const EPN_GETTER: &str = r#"
package extensions
class ExtensionPointName<T : Any>(name: String) {
    val extensionList: List<T> get() = emptyList()
}
"#;

/// The dependent file, mirroring GeneralCommandLineEnvCustomizerService.kt: a companion-rooted
/// chain feeds `forEach`, and the lambda parameter's member call must resolve.
const USE_SITE: &str = r#"
package process
import extensions.ExtensionPointName

interface CommandLineEnvCustomizer {
    fun customizeEnv(x: String)
    companion object {
        val EP_NAME: ExtensionPointName<CommandLineEnvCustomizer> = ExtensionPointName("x")
    }
}

fun test() {
    CommandLineEnvCustomizer.EP_NAME.extensionList.forEach { c -> c.customizeEnv("y") }
}
"#;

#[test]
fn generic_member_prop_substitutes_lambda_param_field_backed() {
    let d = fallback_diagnostics(USE_SITE, EPN_FIELD);
    assert_eq!(d, Vec::<String>::new());
}

#[test]
fn generic_member_prop_substitutes_lambda_param_computed_getter() {
    let d = fallback_diagnostics(USE_SITE, EPN_GETTER);
    assert_eq!(d, Vec::<String>::new());
}

/// The FocusedComponentProvider.kt shape: a `for` loop over the same property binds its loop
/// variable to the substituted element type (read from inside the companion, as the real
/// `findFocusedComponent` does).
const FOR_LOOP: &str = r#"
package process
import extensions.ExtensionPointName

interface FocusedComponentProvider {
    fun getFocusedComponent(): String
    companion object {
        val EP_NAME: ExtensionPointName<FocusedComponentProvider> = ExtensionPointName("x")
        fun findFocusedComponent(): String {
            for (provider in EP_NAME.extensionList) {
                val component = provider.getFocusedComponent()
                if (component.isNotEmpty()) return component
            }
            return ""
        }
    }
}
"#;

#[test]
fn generic_member_prop_substitutes_for_loop_variable() {
    let d = fallback_diagnostics(FOR_LOOP, EPN_FIELD);
    assert_eq!(d, Vec::<String>::new());
    let d = fallback_diagnostics(FOR_LOOP, EPN_GETTER);
    assert_eq!(d, Vec::<String>::new());
}

/// No companion involved: the same substitution applies when the chain roots at a top-level val.
const TOPLEVEL_USE_SITE: &str = r#"
package process
import extensions.ExtensionPointName

interface CommandLineEnvCustomizer {
    fun customizeEnv(x: String)
}

val EP_NAME: ExtensionPointName<CommandLineEnvCustomizer> = ExtensionPointName("x")

fun test() {
    EP_NAME.extensionList.forEach { c -> c.customizeEnv("y") }
}
"#;

#[test]
fn generic_member_prop_substitutes_toplevel_rooted_chain() {
    let d = fallback_diagnostics(TOPLEVEL_USE_SITE, EPN_GETTER);
    assert_eq!(d, Vec::<String>::new());
}

/// The substitution must be exact, not just non-`Any`: reading the element into an incompatible
/// type stays an error (a collapsed `Any` element would silently accept this).
const MISMATCH_USE_SITE: &str = r#"
package process
import extensions.ExtensionPointName

interface CommandLineEnvCustomizer {
    fun customizeEnv(x: String)
    companion object {
        val EP_NAME: ExtensionPointName<CommandLineEnvCustomizer> = ExtensionPointName("x")
    }
}

fun test() {
    val wrong: String = CommandLineEnvCustomizer.EP_NAME.extensionList.get(0)
}
"#;

#[test]
fn generic_member_prop_substituted_element_is_exact() {
    let d = fallback_diagnostics(MISMATCH_USE_SITE, EPN_GETTER);
    assert_eq!(
        d.len(),
        1,
        "expected exactly one mismatch diagnostic, got: {d:?}"
    );
    assert!(
        d[0].contains("type mismatch") || d[0].contains("initializer type mismatch"),
        "expected a type-mismatch diagnostic, got: {d:?}"
    );
}
