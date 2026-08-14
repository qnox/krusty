//! A classpath function with BOTH a defaulted parameter and a `vararg` (`fun f(a: Int = 0,
//! vararg xs: T)`, called as `f()`) failed to resolve: `default_arg_mapping` treated the vararg
//! slot as a REQUIRED parameter (metadata never sets `declares_default_value` on a vararg — it is
//! implicitly omittable), so the `$default` candidate was rejected and the call reported
//! `unresolved function 'f'`.

use super::common;

const LIB: &str = "package lib\n\
    fun <T : Any> omittable(name: String? = null, vararg more: T): String = \"ok\"\n";

fn assert_accepted(name: &str, main: &str) {
    let Some(diagnostics) = common::checker_diags_against("default_vararg", LIB, main) else {
        eprintln!("skipping: no kotlinc/stdlib toolchain");
        return;
    };
    assert!(
        diagnostics.is_empty(),
        "{name}: unexpected diagnostics: {diagnostics:?}"
    );
}

/// Reference-compiled dependency variant: these cases consume kotlinc-emitted metadata
/// shapes krusty does not produce yet (see `common::compile_lib_ref`).
fn assert_accepted_ref(name: &str, main: &str) {
    let Some(diagnostics) = common::checker_diags_against_ref("default_vararg", LIB, main) else {
        eprintln!("skipping: no kotlinc/stdlib toolchain");
        return;
    };
    assert!(
        diagnostics.is_empty(),
        "{name}: unexpected diagnostics: {diagnostics:?}"
    );
}

#[test]
fn default_and_vararg_omitted_with_type_arg() {
    assert_accepted_ref(
        "type arg, no value args",
        "import lib.omittable\nfun box() { omittable<Any>() }\n",
    );
}

#[test]
fn default_and_vararg_omitted_runs_on_jvm() {
    // End-to-end: the `$default` emit shape (omitted vararg slot → empty array, no mask bit) must
    // also lower correctly, not just resolve.
    common::expect_box_ok_against_ref(
        "default_vararg_box",
        LIB,
        "import lib.omittable\nfun box(): String = if (omittable<Any>() == \"ok\") \"OK\" else \"fail\"\n",
    );
}

#[test]
fn default_and_vararg_omitted_plain() {
    assert_accepted_ref(
        "no args at all",
        "import lib.omittable\nfun box() { omittable() }\n",
    );
}

#[test]
fn vararg_used_default_given() {
    // Regression guard: supplies every parameter, so it passes through the base overload's vararg
    // machinery and never touches the changed guard — it passed pre-fix too.
    assert_accepted(
        "first arg + vararg elements",
        "import lib.omittable\nfun box() { omittable(\"n\", Any()) }\n",
    );
}
