//! The generic `Signature` of a suspend function's continuation. kotlinc writes the continuation's
//! `in` projection as `? super R` except over `Any`, where it is redundant: a suspend function
//! returning `Any` or `Any?`, and a suspend function type with that result, sign
//! `Continuation<Object>`.

use super::common;

#[test]
fn a_continuation_over_any_has_no_wildcard() {
    const SRC: &str = "class Token\n\
\n\
suspend fun anything(): Any? = null\n\
\n\
suspend fun something(): Any = Token()\n\
\n\
suspend fun token(): Token = Token()\n\
\n\
fun takes(f: suspend (Token) -> Any) {}\n\
\n\
fun passes(f: suspend () -> Any?): suspend () -> Any? = f\n";
    common::byte_diff_against_kotlinc("ContinuationOverAny", SRC, "ContinuationOverAnyKt")
        .expect("reference kotlinc is provisioned")
        .expect("ContinuationOverAnyKt byte-identical to kotlinc");
}

/// A `Nothing` result is the continuation's argument like any other: `Continuation<in T>` writes it
/// as a star, `Continuation<*>`, and a `Nothing?` result leaves the continuation raw. A
/// `Lkotlin/Nothing;` argument names a class no loader can resolve, so reflection over the method
/// (`getGenericParameterTypes`, class retransformation) fails with `TypeNotPresentException`. A suspend
/// function type keeps its generic `FunctionN` supertype: its own result is `Any?`.
#[test]
fn a_nothing_result_is_a_star_continuation() {
    const SRC: &str = "class Failing {\n\
    private suspend fun fail(message: String): Nothing = throw IllegalStateException(message)\n\
\n\
    suspend fun rethrow(): Nothing = fail(\"rethrow\")\n\
\n\
    suspend fun absent(): Nothing? = null\n\
\n\
    suspend fun <T> generic(value: T): Nothing = throw Error(value.toString())\n\
\n\
    fun takes(f: suspend () -> Nothing) {}\n\
\n\
    fun takesNullable(f: suspend () -> Nothing?) {}\n\
\n\
    fun gives(): suspend (Int) -> Nothing = TODO()\n\
}\n\
\n\
suspend fun top(): Nothing = throw Error()\n";
    for class in ["Failing", "SuspendNothingKt"] {
        common::byte_diff_against_kotlinc_cp("SuspendNothing", SRC, class, &[common::stdlib_jar()])
            .expect("reference kotlinc is provisioned")
            .unwrap_or_else(|diff| panic!("{class} byte-identical to kotlinc: {diff}"));
    }
}

/// A suspend lambda whose result is `Nothing` implements `Function1<Continuation<*>, Object>`, and
/// its `create`/`invoke` sign the continuation as a star. Over a `Nothing?` result the continuation
/// is raw, and an `invoke` left with no generic structure has no `Signature`.
#[test]
fn a_suspend_lambda_returning_nothing_has_a_star_continuation() {
    const SRC: &str = "fun fails(): suspend () -> Nothing = { throw Error() }\n\
\n\
fun maybe(): suspend () -> Nothing? = { null }\n\
\n\
fun withArgument(): suspend (String) -> Nothing = { throw Error(it) }\n";
    for class in [
        "SuspendLambdaNothingKt",
        "SuspendLambdaNothingKt$fails$1",
        "SuspendLambdaNothingKt$maybe$1",
        "SuspendLambdaNothingKt$withArgument$1",
    ] {
        common::byte_diff_against_kotlinc_cp(
            "SuspendLambdaNothing",
            SRC,
            class,
            &[common::stdlib_jar()],
        )
        .expect("reference kotlinc is provisioned")
        .unwrap_or_else(|diff| panic!("{class} byte-identical to kotlinc: {diff}"));
    }
}
