//! Import-scoped resolution of TOP-LEVEL functions and EXTENSIONS conforms to kotlinc: an unqualified
//! call binds ONLY to a same-package / imported / default function — NOT to an arbitrary classpath
//! function of that name. A dependency library declares both in a NON-default package `mylib`; the
//! consumer must import them, exactly as kotlinc requires.

use super::common;

const LIB: &str = "package mylib\n\
    fun provide(): String = \"OK\"\n\
    fun String.tagged(): String = this + \"!\"\n";

fn mentions(diags: &[String], needle: &str) -> bool {
    diags
        .iter()
        .any(|d| d.contains(needle) || d.to_lowercase().contains("unresolved"))
}

#[test]
fn unimported_top_level_function_is_unresolved() {
    // No `import mylib.provide` — kotlinc requires it, so a bare `provide()` must NOT resolve to the
    // classpath function (the over-permissive whole-classpath lookup would have found it).
    let main = "fun box(): String = provide()\n";
    let Some(diags) = common::checker_diags_against("scope_tl_neg", LIB, main) else {
        return; // toolchain not provisioned
    };
    assert!(
        !diags.is_empty() && mentions(&diags, "provide"),
        "an un-imported top-level function must be unresolved, got {diags:?}"
    );
}

#[test]
fn imported_top_level_function_resolves_and_runs() {
    let main = "import mylib.provide\n\
        fun box(): String = provide()\n";
    let Some(out) = common::run_box_against("scope_tl_pos", LIB, main) else {
        return;
    };
    assert_eq!(
        out, "OK",
        "an imported top-level function resolves and runs"
    );
}

#[test]
fn unimported_extension_is_unresolved() {
    let main = "fun box(): String = \"x\".tagged()\n";
    let Some(diags) = common::checker_diags_against("scope_ext_neg", LIB, main) else {
        return;
    };
    assert!(
        !diags.is_empty() && mentions(&diags, "tagged"),
        "an un-imported extension must be unresolved, got {diags:?}"
    );
}

#[test]
fn imported_extension_resolves_and_runs() {
    let main = "import mylib.tagged\n\
        fun box(): String = if (\"O\".tagged() == \"O!\") \"OK\" else \"NO\"\n";
    let Some(out) = common::run_box_against("scope_ext_pos", LIB, main) else {
        return;
    };
    assert_eq!(out, "OK", "an imported extension resolves and runs");
}
