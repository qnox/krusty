//! A FULLY-QUALIFIED call to a library top-level function with an EXPLICIT type argument and a
//! SYNTACTIC trailing lambda, where a defaulted parameter precedes the trailing `block` parameter —
//! `kotlin.test.assertFailsWith<E> { … }` spelled without an import. kotlinc accepts this (the
//! `message: String?` defaults, the lambda binds the trailing `block` parameter, and the explicit
//! type argument fixes `T`), but krusty mapped the trailing lambda positionally onto the FIRST
//! parameter and reported "argument type mismatch: actual type is '() -> X', but 'String' was
//! expected".
use super::common;

const LIB: &str = "package pkg\n\
    fun <T> fw(message: String? = null, block: () -> T): T {\n\
    \x20 if (message != null) throw AssertionError(message)\n\
    \x20 return block()\n\
    }\n\
    inline fun <reified T : Throwable> failsWith(message: String? = null, block: () -> Unit): T {\n\
    \x20 try { block() } catch (e: Throwable) {\n\
    \x20   if (e is T) return e\n\
    \x20   throw AssertionError(\"unexpected: \" + e)\n\
    \x20 }\n\
    \x20 throw AssertionError(message ?: \"expected an exception\")\n\
    }\n\
    fun <T> g(a: Int = 0, b: String, block: () -> T): T {\n\
    \x20 if (a != 0) throw AssertionError(b)\n\
    \x20 return block()\n\
    }\n";

#[test]
fn fq_targ_trailing_lambda_generic_result() {
    // Plain generic: the explicit type argument plus the trailing lambda must skip the defaulted
    // leading `message` parameter.
    const MAIN: &str = "fun box(): String {\n\
        \x20 val n = pkg.fw<Int> { 21 + 21 }\n\
        \x20 return if (n == 42) \"OK\" else \"fail: $n\"\n\
        }\n";
    let Some(out) = common::expect_box_run_against("fq_targ_trailing_lambda", LIB, MAIN) else {
        return;
    };
    assert_eq!(out, "OK");
}

#[test]
fn fq_targ_trailing_lambda_checker_clean() {
    // The reported symptom was a checker diagnostic ("argument type mismatch … 'String' was
    // expected"): assert the FQ + explicit-targ + trailing-lambda call checks clean.
    const MAIN: &str = "fun box(): String {\n\
        \x20 val n = pkg.fw<Int> { 42 }\n\
        \x20 return n.toString()\n\
        }\n";
    let Some(diags) = common::checker_diags_against("fq_targ_trailing_lambda", LIB, MAIN) else {
        return;
    };
    assert!(diags.is_empty(), "expected clean check, got: {diags:?}");
}

#[test]
fn fq_targ_trailing_lambda_reified_checker_clean() {
    // The assertFailsWith shape against a USER library: reified inline, bound `T : Throwable`,
    // explicit type argument, trailing lambda after a defaulted `message`. The checker must accept
    // it (end-to-end lowering of a custom reified `$default` splice is an orthogonal, shared gap —
    // the bare-name `import`ed spelling bails the same way, never miscompiles).
    const MAIN: &str = "fun box(): String {\n\
        \x20 val e = pkg.failsWith<IllegalStateException> { throw IllegalStateException(\"boom\") }\n\
        \x20 return if (e.message == \"boom\") \"OK\" else \"fail: ${e.message}\"\n\
        }\n";
    let Some(diags) = common::checker_diags_against("fq_targ_trailing_lambda", LIB, MAIN) else {
        return;
    };
    assert!(diags.is_empty(), "expected clean check, got: {diags:?}");
}

#[test]
fn fq_targ_named_arg_and_trailing_lambda_checker_clean() {
    // The NAMED-argument channel adjacent to the new slot-mapped branch: `message` labelled, lambda
    // trailing — resolves through `resolved_slots`, not the new mapping, and must stay clean.
    // (`message = null` is a SEPARATE pre-existing gap: the named channel loses the parameter's
    // metadata `String?` nullability and rejects the null.)
    const MAIN: &str = "fun box(): String {\n\
        \x20 val n = pkg.fw<Int>(message = \"m\") { 42 }\n\
        \x20 return n.toString()\n\
        }\n";
    let Some(diags) = common::checker_diags_against("fq_targ_trailing_lambda", LIB, MAIN) else {
        return;
    };
    assert!(diags.is_empty(), "expected clean check, got: {diags:?}");
}

#[test]
fn fq_targ_trailing_lambda_mid_omission_rejected() {
    // The border the new branch must NOT mis-accept: a positional argument cannot skip a leading
    // defaulted parameter (`\"x\"` pairs with `a: Int`, kotlinc rejects). Guard against the
    // slot-mapped branch ever swallowing the mismatch.
    const MAIN: &str = "fun box(): String {\n\
        \x20 val n = pkg.g<Int>(\"x\") { 42 }\n\
        \x20 return n.toString()\n\
        }\n";
    let Some(diags) = common::checker_diags_against("fq_targ_trailing_lambda", LIB, MAIN) else {
        return;
    };
    assert!(
        !diags.is_empty(),
        "expected a diagnostic for the positional String-into-Int argument"
    );
}

/// Compile-and-run `main` with kotlin-test on the classpath. An unprovisioned kotlin-test jar is a
/// FAILURE, never a skip — these are the verbatim-bug regression tests, and a silent pass-as-skip
/// would hide exactly the regression they pin (see the toolchain-accessor rule in `common`).
fn run_with_kotlin_test(main: &str) -> String {
    let test_jar = common::kotlin_test_jar()
        .expect("no kotlin-test jar found; provision the reference toolchain (`just` fetches it)");
    let stdlib = common::stdlib_jar();
    let jdk = common::jdk_modules();
    common::expect_box_run(
        main,
        "Main",
        &[stdlib, test_jar, jdk.clone()],
        Some(jdk.as_path()),
    )
}

#[test]
fn fq_assert_fails_with_targ_trailing_lambda() {
    // The reported bug verbatim: `kotlin.test.assertFailsWith<E> { … }` spelled fully qualified,
    // no import — the explicit type argument fixes `T`, the trailing lambda binds `block`, and the
    // defaulted `message: String?` is omitted. Lowered by the reified assertFailsWith intrinsic.
    const MAIN: &str = "fun box(): String {\n\
        \x20 val e = kotlin.test.assertFailsWith<IllegalStateException> { throw IllegalStateException(\"boom\") }\n\
        \x20 return if (e.message == \"boom\") \"OK\" else \"fail: ${e.message}\"\n\
        }\n";
    assert_eq!(run_with_kotlin_test(MAIN), "OK");
}

#[test]
fn fq_assert_fails_with_message_and_trailing_lambda() {
    // Same call with the `message` argument PROVIDED: full positional arity plus the trailing
    // lambda — the pre-existing index-for-index pairing must keep working.
    const MAIN: &str = "fun box(): String {\n\
        \x20 val e = kotlin.test.assertFailsWith<IllegalStateException>(\"m\") { throw IllegalStateException(\"boom\") }\n\
        \x20 return if (e.message == \"boom\") \"OK\" else \"fail: ${e.message}\"\n\
        }\n";
    assert_eq!(run_with_kotlin_test(MAIN), "OK");
}
