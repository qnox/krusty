//! A classpath top-level function with a NON-FINAL `vararg` followed by a required parameter
//! (`fun f(vararg parts: String, block: () -> String)`) failed to resolve when the trailing
//! parameter was supplied by name: the positional shape selector only tried the LAST parameter
//! as the vararg slot, and the non-final form was rejected because the trailing parameter has no
//! default — even though the named argument already claimed that slot. kotlinc accepts and runs
//! the call.

use super::common;

const LIB: &str = "package lib\n\
    fun assembleNc(vararg parts: String, block: () -> String): String = \
    parts.joinToString(\"\") + block()\n";

fn assert_accepted(name: &str, main: &str) {
    let Some(diagnostics) = common::checker_diags_against("nonfinal_vararg_named", LIB, main)
    else {
        eprintln!("skipping: no kotlinc/stdlib toolchain");
        return;
    };
    assert!(
        diagnostics.is_empty(),
        "{name}: unexpected diagnostics: {diagnostics:?}"
    );
}

#[test]
fn nonfinal_vararg_with_named_trailing_param() {
    assert_accepted(
        "vararg elements + named trailing param",
        "import lib.assembleNc\nfun probe(): String = assembleNc(\"a\", \"b\", block = { \"x\" })\n",
    );
}

#[test]
fn nonfinal_vararg_empty_with_named_trailing_param() {
    assert_accepted(
        "no vararg elements + named trailing param",
        "import lib.assembleNc\nfun probe(): String = assembleNc(block = { \"x\" })\n",
    );
}

#[test]
fn nonfinal_vararg_trailing_lambda() {
    // kotlinc also accepts the trailing-lambda spelling: the lambda binds to the last parameter
    // and the positional prefix fills the vararg.
    assert_accepted(
        "vararg elements + trailing lambda",
        "import lib.assembleNc\nfun probe(): String = assembleNc(\"a\", \"b\") { \"x\" }\n",
    );
}

#[test]
fn nonfinal_vararg_named_runs_on_jvm() {
    common::expect_box_ok_against(
        "nonfinal_vararg_named_box",
        LIB,
        "import lib.assembleNc\n\
         fun box(): String = if (assembleNc(\"a\", \"b\", block = { \"x\" }) == \"abx\") \"OK\" else \"fail\"\n",
    );
}
