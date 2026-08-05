//! Member call on the lambda parameter of `forEach` whose RECEIVER chain is rooted at a
//! COMPANION-object val: `CommandLineEnvCustomizer.EP_NAME.extensionList.forEach { c ->
//! c.customizeEnv("y") }`. The companion-rooted receiver chain feeding a call with a lambda
//! argument never propagated the receiver's type into the lambda parameter, so member calls on the
//! parameter reported `Unresolved reference 'customizeEnv'.`. Chains rooted at a top-level val
//! typed fine, and companion-rooted chains WITHOUT a lambda call typed fine — only the combination
//! (companion val root + receiver chain + lambda-taking call) lost the lambda parameter type.
//!
//! Found on intellij-community's
//! platform/platform-api/src/com/intellij/execution/process/GeneralCommandLineEnvCustomizerService.kt:12
//! (`CommandLineEnvCustomizer.EP_NAME.extensionList.forEach { customizer -> … }`); kotlinc
//! compiles the original file cleanly.
//!
//! The tests go through `analyze_source_set_prefix_with_features_trimmed` — the exact entry the LSP
//! analysis worker uses (crates/krusty-lsp/src/worker.rs) — with the generic `ExtensionPointName`
//! support class in an inferred support file, mirroring how the real one arrives via the classpath.

use super::common;
use krusty::jvm::classpath::Classpath;

/// Check `checked` against inferred support files through the LSP worker's analysis entry. Returns
/// the checker's diagnostic messages for the checked file.
fn worker_diagnostics(checked: &str, inferred: &[&str]) -> Vec<String> {
    let stdlib = common::stdlib_jar();
    let jdk = common::jdk_modules();
    let cp = std::rc::Rc::new(Classpath::new(vec![stdlib, jdk]));
    let platform = Box::new(krusty::jvm::jvm_libraries::JvmLibraries::new(cp));
    let mut inputs = vec![krusty::frontend::SourceInput::kotlin(checked)];
    inputs.extend(
        inferred
            .iter()
            .map(|s| krusty::frontend::SourceInput::kotlin(s)),
    );
    let mut diags = krusty::diag::DiagSink::new();
    let _ = krusty::frontend::analyze_source_set_prefix_with_features_trimmed(
        &inputs,
        1,
        inputs.len(),
        platform,
        &krusty::features::LangFeatures::new(),
        &mut diags,
    );
    diags.diags.iter().map(|d| d.msg.clone()).collect()
}

/// The generic support class, mirroring intellij's `ExtensionPointName<T>` (arrives via the
/// classpath in the real workspace; here via an inferred support file).
const EPN: &str = r#"
package extensions
class ExtensionPointName<T : Any>(name: String) {
    val extensionList: List<T> get() = emptyList()
}
"#;

/// The use site, mirroring GeneralCommandLineEnvCustomizerService.kt: the companion-rooted generic
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
fn companion_rooted_generic_chain_lambda_member_call() {
    let d = worker_diagnostics(USE_SITE, &[EPN]);
    assert_eq!(d, Vec::<String>::new());
}

/// Same repro with everything in one file, so the failure cannot be blamed on cross-file linking.
const ALL_ONE_FILE: &str = r#"
package process

class ExtensionPointName<T : Any>(name: String) {
    val extensionList: List<T> get() = emptyList()
}

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
fn companion_rooted_generic_chain_lambda_member_call_same_file() {
    let d = worker_diagnostics(ALL_ONE_FILE, &[]);
    assert_eq!(d, Vec::<String>::new());
}

/// Non-generic companion-rooted chain: `Iface.H.items.forEach { s -> s.length }` failed the same
/// way, so the fix must not depend on generics either.
const COMPANION_NONGENERIC: &str = r#"
package process

class Holder {
    val items: List<String> = emptyList()
}

interface Iface {
    fun customizeEnv(x: String)
    companion object {
        val H: Holder = Holder()
    }
}

fun test() {
    Iface.H.items.forEach { s -> s.length }
}
"#;

#[test]
fn companion_rooted_nongeneric_chain_lambda_member_call() {
    let d = worker_diagnostics(COMPANION_NONGENERIC, &[]);
    assert_eq!(d, Vec::<String>::new());
}

/// Control that always passed: the identical chain rooted at a TOP-LEVEL val types the lambda
/// parameter fine.
const TWO_HOP_TOPLEVEL: &str = r#"
package process

class Holder {
    val items: List<String> = emptyList()
}

val H: Holder = Holder()

fun test() {
    H.items.forEach { s -> s.length }
}
"#;

#[test]
fn toplevel_rooted_chain_lambda_member_call_control() {
    let d = worker_diagnostics(TWO_HOP_TOPLEVEL, &[]);
    assert_eq!(d, Vec::<String>::new());
}

/// Control that always passed: a companion-rooted generic chain WITHOUT a lambda call projects the
/// type arguments fine.
const COMPANION_PROJ_CHECK: &str = r#"
package process

class ExtensionPointName<T : Any>(name: String) {
    val extensionList: List<T> get() = emptyList()
}

interface CommandLineEnvCustomizer {
    fun customizeEnv(x: String)
    companion object {
        val EP_NAME: ExtensionPointName<CommandLineEnvCustomizer> = ExtensionPointName("x")
    }
}

fun test() {
    val l: List<CommandLineEnvCustomizer> = CommandLineEnvCustomizer.EP_NAME.extensionList
}
"#;

#[test]
fn companion_rooted_generic_chain_without_lambda_control() {
    let d = worker_diagnostics(COMPANION_PROJ_CHECK, &[]);
    assert_eq!(d, Vec::<String>::new());
}
