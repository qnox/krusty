//! kotlinc brackets every lambda it inlines into a classpath inline body with
//! `InlineMarker.beforeInlineCall`/`afterInlineCall`, and FixStack in the finished caller stores
//! whatever is on the operand stack under the lambda into fresh locals, reloading it under the
//! lambda's result. That is the body's own operands (stdlib `map`'s inlined `mapTo` has the
//! destination on the stack at the transform's `invoke`) and the caller's, pushed before the inline
//! call. A lambda with a `try` must be entered on that saved, empty stack: its handler starts with
//! only the exception, or the paths join with different stacks (the private corpus crashed on
//! exactly this: "an inlined body's stack was already followed: Mismatch").

use super::common;

/// A spliced `runCatching` in a lambda invoked over a non-empty stack in the inline body.
const RUN_CATCHING_LAMBDA: &str = r#"
fun parsed(values: List<String>): List<Int> =
    values.map { text -> runCatching { text.toInt() }.getOrDefault(-1) }

fun box(): String {
    val result = parsed(listOf("1", "x", "3"))
    return if (result == listOf(1, -1, 3)) "OK" else "fail: $result"
}
"#;

#[test]
fn a_run_catching_lambda_inlined_over_a_stacked_value_runs() {
    common::expect_box_ok_with_stdlib(RUN_CATCHING_LAMBDA, "InlinedLambdaRunCatchingSpill");
}

/// A source `try`/`catch` in the lambda: the handler must catch on the throwing element only.
const TRY_CATCH_LAMBDA: &str = r#"
fun parsed(values: List<String>): List<Int> =
    values.map { text ->
        try {
            text.toInt()
        } catch (e: NumberFormatException) {
            -1
        }
    }

fun box(): String {
    val result = parsed(listOf("7", "seven", "9"))
    return if (result == listOf(7, -1, 9)) "OK" else "fail: $result"
}
"#;

#[test]
fn a_try_catch_lambda_inlined_over_a_stacked_value_runs() {
    common::expect_box_ok_with_stdlib(TRY_CATCH_LAMBDA, "InlinedLambdaTryCatchSpill");
}

/// The inline call is a later argument, so caller operands are pushed before it.
const TRY_CATCH_LAMBDA_AFTER_ARGUMENTS: &str = r#"
fun consume(n: Int, total: Long, values: List<Int>): String = "$n$total$values"

fun parsed(values: List<String>): String =
    consume(1, 2L, values.map { text ->
        try {
            text.toInt()
        } catch (e: NumberFormatException) {
            -1
        }
    })

fun box(): String {
    val result = parsed(listOf("7", "seven", "9"))
    return if (result == "12[7, -1, 9]") "OK" else "fail: $result"
}
"#;

#[test]
fn a_try_catch_lambda_inlined_after_caller_arguments_runs() {
    common::expect_box_ok_with_stdlib(
        TRY_CATCH_LAMBDA_AFTER_ARGUMENTS,
        "InlinedLambdaTryCatchAfterArguments",
    );
}

/// A kotlinc-compiled inline function whose lambda is invoked with a `StringBuilder` on the stack.
/// It is `@InlineOnly`, so it has no callable fallback: a run proves the body was inlined.
const LIB: &str = r#"
@file:Suppress("INVISIBLE_MEMBER", "INVISIBLE_REFERENCE")

@kotlin.internal.InlineOnly
inline fun tagged(prefix: String, block: () -> Int): String = prefix + block()
"#;

/// The inline call is a constructor argument, so the caller's `new Holder; dup` is under the whole
/// inlined body: the bracket around the lambda saves the caller's operands and the body's.
const TRY_CATCH_LAMBDA_UNDER_A_CONSTRUCTION: &str = r#"
class Holder(val value: String)

fun parsed(text: String): Holder =
    Holder(tagged("v") {
        try {
            text.toInt()
        } catch (e: NumberFormatException) {
            -1
        }
    })

fun box(): String {
    val result = parsed("7").value + parsed("seven").value
    return if (result == "v7v-1") "OK" else "fail: $result"
}
"#;

#[test]
fn a_try_catch_lambda_inlined_under_a_caller_construction_runs() {
    let library = common::kotlinc_library(LIB).expect("reference compiler unavailable");
    let jdk = common::jdk_modules();
    assert_eq!(
        common::expect_box_run(
            TRY_CATCH_LAMBDA_UNDER_A_CONSTRUCTION,
            "InlinedLambdaTryCatchUnderConstruction",
            &[library, common::stdlib_jar()],
            Some(jdk.as_path()),
        ),
        "OK"
    );
}

/// The same caller operands under a lambda without a `try`, saved and reloaded as kotlinc does.
const LAMBDA_UNDER_A_CONSTRUCTION: &str = r#"
class Holder(val value: String)

fun parsed(text: String): Holder = Holder(tagged("v") { text.length })
"#;

#[test]
fn a_lambda_inlined_under_a_caller_construction_spills_like_kotlinc() {
    let library = common::kotlinc_library(LIB).expect("reference compiler unavailable");
    common::assert_class_code_matches_kotlinc_jdk_cp(
        "InlinedLambdaUnderConstructionParity",
        LAMBDA_UNDER_A_CONSTRUCTION,
        "InlinedLambdaUnderConstructionParityKt",
        std::slice::from_ref(&library),
    );
}

/// The same save and reload happens around a lambda with no `try` at all: the destination goes to
/// a local above the inlined frame and comes back under the lambda's result (`aload; swap`).
const PLAIN_LAMBDA: &str = r#"
fun lengths(values: List<String>): List<Int> = values.map { it.length + 1 }

fun box(): String {
    val result = lengths(listOf("a", "bb"))
    return if (result == listOf(2, 3)) "OK" else "fail: $result"
}
"#;

#[test]
fn a_plain_lambda_inlined_over_a_stacked_value_spills_like_kotlinc() {
    common::assert_class_code_matches_kotlinc(
        "InlinedLambdaPlainSpillParity",
        PLAIN_LAMBDA,
        "InlinedLambdaPlainSpillParityKt",
    );
}
