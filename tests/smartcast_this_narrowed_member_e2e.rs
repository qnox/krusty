//! Smart-cast of the EXTENSION RECEIVER `this` vs a same-named plain top-level function elsewhere in
//! the module: inside `fun HyperlinkInfo.navigate(...)`, `if (this is HyperlinkInfoBase) { … }`
//! narrows `this` to the subtype, and the subtype's 2-arg `navigate(Project, RelativePoint?)` MEMBER
//! must win overload resolution — members of an implicit receiver shadow top-level functions in
//! kotlinc's scope order. The narrowed-`this` member retry used to sit behind a
//! `!module_declares(name)` gate meant for top-level resolution, so ANY plain top-level `navigate`
//! declared anywhere in the analyzed module (any package, any signature) suppressed the member retry
//! and the call fell through to `Too many arguments for 'fun navigate(project: Project): Unit'.`.
//!
//! Found on intellij-community's
//! platform/platform-api/src/com/intellij/execution/filters/HyperlinkInfoBase.kt:49; the suppressing
//! top-level is `internal fun navigate(project: Project, requestFocus: Boolean, …)` in the unrelated
//! com/intellij/util/OpenSourceUtil.kt. Single-file analysis never hit it (no top-level `navigate`),
//! which is why every small repro passes and only the full-workspace LSP scan reports the arity error.
//!
//! The tests go through `analyze_source_set_prefix_with_features_trimmed` — the exact entry the LSP
//! analysis worker uses (crates/krusty-lsp/src/worker.rs) — with the checked file first and the
//! "poison" top-level in an inferred support file.

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

/// The checked half, mirroring HyperlinkInfoBase.kt: an extension on the interface, smart-cast of
/// the receiver, then a 2-arg member call on the narrowed subtype.
const CHECKED: &str = r#"
package filters

class Project
open class RelativePoint

fun interface HyperlinkInfo {
    fun navigate(project: Project)
}

abstract class HyperlinkInfoBase : HyperlinkInfo {
    abstract fun navigate(project: Project, point: RelativePoint?)
    override fun navigate(project: Project) {
        navigate(project, null)
    }
}

fun HyperlinkInfo.navigate(project: Project, flag: Boolean) {
    if (this is HyperlinkInfoBase && flag) {
        navigate(project, RelativePoint())
    } else {
        navigate(project)
    }
}

fun box(): String = "OK"
"#;

/// The poison pill, mirroring OpenSourceUtil.kt: a plain top-level `navigate` in an UNRELATED
/// package with an UNRELATED signature.
const POISON: &str = r#"
package util

fun navigate(x: Int, requestFocus: Boolean): Boolean = requestFocus
"#;

#[test]
fn narrowed_this_member_not_suppressed_by_toplevel() {
    // The exact HyperlinkInfoBase.kt:49 + OpenSourceUtil.kt shape: the narrowed receiver's 2-arg
    // member resolves — previously `Too many arguments for 'fun navigate(project: Project): Unit'.`.
    assert_eq!(worker_diagnostics(CHECKED, &[POISON]), Vec::<String>::new());
}

#[test]
fn narrowed_this_member_resolves_without_toplevel() {
    // Control: the same checked file alone (no poison top-level) always resolved.
    assert_eq!(worker_diagnostics(CHECKED, &[]), Vec::<String>::new());
}

#[test]
fn genuine_toplevel_call_still_resolves_with_member_present() {
    // kotlinc parity, negative direction: a call that fits ONLY the top-level must still bind the
    // top-level — hoisting the member retry must not make members shadow genuinely top-level calls.
    let d = worker_diagnostics(
        r#"
package filters

class Project

fun navigate(x: Int): Boolean = true

fun call(): Boolean = navigate(1)
"#,
        &[],
    );
    assert_eq!(d, Vec::<String>::new());
}

#[test]
fn inapplicable_narrowed_member_falls_through_to_toplevel() {
    // Smart cast active + narrowed member NOT applicable + module top-level applicable: the retry
    // probe must silently return None and the call must still bind the top-level (kotlinc parity) —
    // a steal here would be silent otherwise.
    let d = worker_diagnostics(
        r#"
package filters

class Project
open class RelativePoint

fun interface HyperlinkInfo {
    fun navigate(project: Project)
}

abstract class HyperlinkInfoBase : HyperlinkInfo {
    abstract fun navigate(project: Project, point: RelativePoint?)
}

fun navigate(x: Int): Boolean = true

fun HyperlinkInfo.f(project: Project, flag: Boolean): Boolean {
    if (this is HyperlinkInfoBase && flag) {
        return navigate(1)
    }
    return false
}
"#,
        &[],
    );
    assert_eq!(d, Vec::<String>::new());
}

#[test]
fn unapplicable_call_still_reports_without_narrowing() {
    // kotlinc parity, error direction: WITHOUT the smart cast (receiver stays the interface), a
    // 3-arg call fits no member, no extension, and no top-level — a diagnostic must still be
    // reported; the hoisted member retry must not mask genuine errors.
    let d = worker_diagnostics(
        r#"
package filters

class Project
open class RelativePoint

fun interface HyperlinkInfo {
    fun navigate(project: Project)
}

fun HyperlinkInfo.navigate(project: Project, flag: Boolean) {
    navigate(project, RelativePoint(), 42)
}
"#,
        &[POISON],
    );
    assert!(
        d.iter().any(|m| m.contains("too many arguments")),
        "expected an arity diagnostic, got: {d:?}"
    );
}
