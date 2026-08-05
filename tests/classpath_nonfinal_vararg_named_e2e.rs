//! A classpath top-level function with a NON-FINAL `vararg` followed by a required parameter
//! (`fun f(vararg parts: String, block: () -> String)`) failed to resolve when the trailing
//! parameter was supplied by name: the positional shape selector only tried the LAST parameter
//! as the vararg slot, and the non-final form was rejected because the trailing parameter has no
//! default — even though the named argument already claimed that slot. kotlinc accepts and runs
//! the call.

use super::common;

const LIB: &str = "package lib\n\
    fun assembleNc(vararg parts: String, block: () -> String): String = \
    parts.joinToString(\"\") + block()\n\
    fun tagged(vararg xs: String, s: String = \"d\"): String = \
    xs.joinToString(\",\") + \"|\" + s\n";

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

#[test]
fn nonfinal_vararg_trailing_lambda_runs_on_jvm() {
    common::expect_box_ok_against(
        "nonfinal_vararg_lambda_box",
        LIB,
        "import lib.assembleNc\n\
         fun box(): String = if (assembleNc(\"a\", \"b\") { \"x\" } == \"abx\") \"OK\" else \"fail\"\n",
    );
}

#[test]
fn nonfinal_vararg_empty_runs_on_jvm() {
    common::expect_box_ok_against(
        "nonfinal_vararg_empty_box",
        LIB,
        "import lib.assembleNc\n\
         fun box(): String = if (assembleNc(block = { \"x\" }) == \"x\") \"OK\" else \"fail\"\n",
    );
}

#[test]
fn defaulted_trailing_param_is_not_filled_positionally() {
    // A DEFAULTED parameter after the vararg is element-first in Kotlin: `tagged("a", "b")` packs
    // BOTH arguments and defaults `s` (kotlinc: "a,b|d"). The positional selector must not bind
    // "b" to `s` — that would answer "a|b". Resolving this element-form call against a classpath
    // top-level `$default` is a separate, pre-existing gap (it reports "unresolved function"),
    // so this guards the binding rule at the point that regressed: the selected shape must not
    // be the parameter-filling one.
    let Some(diagnostics) = common::checker_diags_against(
        "nonfinal_vararg_defaulted_trailing",
        LIB,
        "import lib.tagged\nfun probe(): String = tagged(\"a\", \"b\")\n",
    ) else {
        eprintln!("skipping: no kotlinc/stdlib toolchain");
        return;
    };
    assert!(
        !diagnostics.iter().any(|d| d.contains("argument")),
        "defaulted trailing param must not be bound positionally: {diagnostics:?}"
    );
}

#[test]
fn nonfinal_vararg_spread_resolves() {
    // A SPREAD mixed with elements at the non-final vararg slot resolves (kotlinc accepts and
    // runs it, answering "abcx"). Lowering this UNLABELLED spread spelling is a separate gap:
    // plain-name spread calls are diverted to `lower_spread_call`, which only emits a same-file
    // single-vararg module target and otherwise bails (a skip, never a miscompile) — so this is
    // a checker-level assertion. The LABELLED spread form below does lower and run.
    assert_accepted(
        "spread mixed with elements + trailing lambda",
        "import lib.assembleNc\n\
         fun probe(): String {\n\
         \x20   val more = arrayOf(\"b\", \"c\")\n\
         \x20   return assembleNc(\"a\", *more) { \"x\" }\n\
         }\n",
    );
}

#[test]
fn nonfinal_vararg_spread_named_runs_on_jvm() {
    common::expect_box_ok_against(
        "nonfinal_vararg_spread_named_box",
        LIB,
        "import lib.assembleNc\n\
         fun box(): String {\n\
         \x20   val more = arrayOf(\"b\", \"c\")\n\
         \x20   return if (assembleNc(*more, block = { \"x\" }) == \"bcx\") \"OK\" else \"fail\"\n\
         }\n",
    );
}
