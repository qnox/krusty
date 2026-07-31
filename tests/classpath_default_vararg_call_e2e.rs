//! A classpath function with BOTH a defaulted parameter and a `vararg` (`fun f(a: Int = 0,
//! vararg xs: T)` — e.g. mockk's `mockk(name: String? = null, …, vararg moreInterfaces: KClass<*>,
//! …)`) failed to resolve on a call that omits both: `default_arg_mapping` treated the vararg slot
//! as a REQUIRED parameter (metadata never sets `declares_default_value` on a vararg — it is
//! implicitly omittable), so the `$default` candidate was rejected and the call reported
//! `unresolved function 'f'`.

use super::common;

const LIB: &str = "package lib\n\
    fun <T : Any> mockkish(name: String? = null, vararg more: T): String = \"ok\"\n";

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

#[test]
fn default_and_vararg_omitted_with_type_arg() {
    assert_accepted(
        "type arg, no value args",
        "import lib.mockkish\nfun box() { mockkish<Any>() }\n",
    );
}

#[test]
fn default_and_vararg_omitted_plain() {
    assert_accepted(
        "no args at all",
        "import lib.mockkish\nfun box() { mockkish() }\n",
    );
}

#[test]
fn vararg_used_default_given() {
    assert_accepted(
        "first arg + vararg elements",
        "import lib.mockkish\nfun box() { mockkish(\"n\", Any()) }\n",
    );
}
