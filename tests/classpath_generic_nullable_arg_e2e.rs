use super::common;

// A classpath top-level generic function whose FIRST parameter defaults (`message: String? = null`)
// and whose LAST parameter is a function type — the `kotlin.test.assertFailsWith` shape. A call that
// omits the defaulted parameter and passes only a trailing lambda must slot the lambda against the
// LAST parameter (`block`), not positionally against `message`.
const LIB: &str = "package lib\n\
    fun <T : Throwable> fw(message: String? = null, block: () -> T): T = block()\n";

/// The reported bug: the FULLY-QUALIFIED spelling (`lib.fw<...> { ... }`) slotted the trailing
/// lambda against the defaulted `message` parameter and reported
/// "argument type mismatch: actual type is '() -> ...', but 'String' was expected".
#[test]
fn fq_explicit_targ_trailing_lambda_omitting_defaulted_message() {
    let main = "fun box(): String {\n\
        \x20 val e = lib.fw<IllegalStateException> { IllegalStateException(\"boom\") }\n\
        \x20 return if (e.message == \"boom\") \"OK\" else \"FAIL: ${e.message}\"\n\
        }\n";
    common::expect_box_ok_against("cpgenfqlambda", LIB, main);
}

/// Same shape, checker only: the FQ call must produce no diagnostics.
#[test]
fn fq_explicit_targ_trailing_lambda_checks_clean() {
    let main = "fun probe(): IllegalStateException =\n\
        \x20 lib.fw<IllegalStateException> { IllegalStateException(\"boom\") }\n";
    if let Some(diags) = common::checker_diags_against("cpgenfqdiags", LIB, main) {
        assert!(
            diags.is_empty(),
            "expected clean resolution, got: {diags:#?}"
        );
    }
}

/// The defaulted parameter SUPPLIED positionally ahead of the trailing lambda must still check the
/// prefix against the leading parameters.
#[test]
fn fq_explicit_targ_message_supplied_with_trailing_lambda() {
    let main = "fun box(): String {\n\
        \x20 val e = lib.fw<IllegalStateException>(\"note\") { IllegalStateException(\"boom\") }\n\
        \x20 return if (e.message == \"boom\") \"OK\" else \"FAIL: ${e.message}\"\n\
        }\n";
    common::expect_box_ok_against("cpgenfqmsg", LIB, main);
}

/// The real-world shape the minimization came from: `kotlin.test.assertFailsWith` FQ-qualified with
/// an explicit type argument, a trailing lambda, and the defaulted `message` omitted. Reified, so it
/// lowers through the `assertFailsWith$default` intrinsic, not a direct `invokestatic`.
#[test]
fn fq_kotlin_test_assert_fails_with_explicit_targ_trailing_lambda() {
    let Some(test_jar) = common::kotlin_test_jar() else {
        return;
    };
    let stdlib = common::stdlib_jar();
    let jdk = common::jdk_modules();
    let main = "fun box(): String {\n\
        \x20 val e = kotlin.test.assertFailsWith<IllegalStateException> { error(\"boom\") }\n\
        \x20 return if (e.message == \"boom\") \"OK\" else \"FAIL: ${e.message}\"\n\
        }\n";
    let out = common::expect_box_run(main, "Main", &[stdlib, test_jar], Some(jdk.as_path()));
    assert_eq!(out, "OK");
}

/// The FQ VARARG channel has its own copy of the erased-return coercion gate: a bounded generic
/// vararg (`fun <T : Throwable> pick(vararg xs: T): T`) physically returns the BOUND (`Throwable`),
/// so the packed-vararg call needs the same `checkcast` to the substituted type.
#[test]
fn fq_bounded_generic_vararg_return() {
    let lib = "package lib\n\
        fun <T : Throwable> pick(vararg xs: T): T = xs[0]\n";
    let main = "fun box(): String {\n\
        \x20 val e = lib.pick(IllegalStateException(\"boom\"))\n\
        \x20 return if (e.message == \"boom\") \"OK\" else \"FAIL: ${e.message}\"\n\
        }\n";
    common::expect_box_ok_against("cpgenfqvararg", lib, main);
}

/// Regression guard: the IMPORTED (bare-name) spelling of the same call already resolved through the
/// unqualified channel's trailing-lambda default branch and must stay green.
#[test]
fn imported_explicit_targ_trailing_lambda_omitting_defaulted_message() {
    let main = "import lib.fw\n\
        fun box(): String {\n\
        \x20 val e = fw<IllegalStateException> { IllegalStateException(\"boom\") }\n\
        \x20 return if (e.message == \"boom\") \"OK\" else \"FAIL: ${e.message}\"\n\
        }\n";
    common::expect_box_ok_against("cpgenimplambda", LIB, main);
}
