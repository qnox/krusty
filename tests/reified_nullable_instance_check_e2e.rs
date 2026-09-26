//! `v is T` for a reified `T` substituted with a nullable type accepts `null`.
//!
//! The checker expands a written `v is Token?` into a null test, but a reified type argument is
//! only known once the inline body is substituted. kotlinc's `generateIsCheck` then tests for
//! `null` before the `instanceof` (`dup; ifnull; instanceof; goto; pop; iconst_1`). krusty emitted
//! the bare `instanceof`, so `null is T` was `false` for `T = Token?`, both when a same-file body
//! is inlined and when a classpath body's reified marker is specialized.
use super::common;
use super::common::{compare_with_kotlinc_plugin, method_instructions};

const LIBRARY: &str = "@file:Suppress(\"INVISIBLE_REFERENCE\", \"INVISIBLE_MEMBER\")\n\
package reifiedfixture\n\
@kotlin.internal.InlineOnly\n\
inline fun <reified T> accepts(v: Any?): Boolean = v is T\n\
@kotlin.internal.InlineOnly\n\
inline fun <reified T> rejects(v: Any?): Boolean = v !is T\n";

const CALLERS: &str = "class Token\n\
fun tokenOrNull(v: Any?): Boolean = accepts<Token?>(v)\n\
fun notTokenOrNull(v: Any?): Boolean = rejects<Token?>(v)\n\
fun branch(v: Any?): Int = if (accepts<Token?>(v)) 1 else 2\n\
fun nullAccepted(): Boolean = accepts<Token?>(null)\n\
fun scalarAccepted(n: Int): Boolean = accepts<Int?>(n)\n";

const BOX: &str = "fun box(): String {\n\
    if (!tokenOrNull(null) || !tokenOrNull(Token()) || tokenOrNull(\"x\")) return \"fail is\"\n\
    if (notTokenOrNull(null) || notTokenOrNull(Token()) || !notTokenOrNull(\"x\")) return \"fail !is\"\n\
    if (branch(null) != 1 || branch(Token()) != 1 || branch(\"x\") != 2) return \"fail branch\"\n\
    if (!nullAccepted()) return \"fail null\"\n\
    if (!scalarAccepted(3)) return \"fail scalar\"\n\
    return \"OK\"\n\
}\n";

#[test]
fn same_file_reified_nullable_instance_checks_accept_null() {
    common::expect_box_ok_with_stdlib(
        &format!(
            "inline fun <reified T> accepts(v: Any?): Boolean = v is T\n\
             inline fun <reified T> rejects(v: Any?): Boolean = v !is T\n\
             {CALLERS}{BOX}"
        ),
        "ReifiedNullable",
    );
}

#[test]
fn classpath_reified_nullable_instance_checks_accept_null() {
    let result = common::expect_box_run_against_kotlinc(
        LIBRARY,
        &format!("import reifiedfixture.*\n{CALLERS}{BOX}"),
    )
    .expect("reference kotlinc is provisioned");
    assert_eq!(result, "OK");
}

/// kotlinc's method optimizer folds the check when its operand is known: a boxed `Int` in
/// `scalarAccepted` and a constant `null` in `nullAccepted` become `true`. krusty does not port
/// those passes yet, so both are only run, not compared.
#[test]
fn classpath_reified_nullable_instance_checks_match_kotlinc() {
    let library = common::kotlinc_library(LIBRARY).expect("reference kotlinc is provisioned");
    let built = compare_with_kotlinc_plugin(
        "ReifiedNullable",
        &format!("import reifiedfixture.*\n{CALLERS}"),
        "ReifiedNullableKt",
        &[common::stdlib_jar(), library],
        "25",
        &[],
    )
    .expect("reference kotlinc is provisioned");
    for member in [
        "boolean tokenOrNull(",
        "boolean notTokenOrNull(",
        "int branch(",
    ] {
        let reference = method_instructions(&built.reference, member);
        assert!(!reference.is_empty(), "kotlinc: {member} not found");
        assert_eq!(
            method_instructions(&built.krusty, member),
            reference,
            "{member}"
        );
    }
}
