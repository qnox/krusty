//! A suspend lambda inside an inlined lambda is a class of its own wherever the enclosing lambda's
//! body is emitted. The inlined copy of that body builds the same class from its own captures,
//! never a `LambdaMetafactory` value over the class's `invokeSuspend`. The inline function is this
//! repository's own, compiled by the reference compiler.

use super::common;

const LIB: &str = r#"
inline fun <R> runIt(block: () -> R): R = block()
"#;

const MAIN: &str = r#"
import kotlin.coroutines.*

fun caller(block: suspend () -> String): String {
    var result = "FAIL"
    block.startCoroutine(Continuation(EmptyCoroutineContext) { result = it.getOrThrow() })
    return result
}

suspend fun make(suffix: String): String = "O$suffix"

fun box(): String {
    val suffix = "K"
    return runIt { caller { make(suffix) } }
}
"#;

#[test]
fn a_suspend_lambda_in_an_inlined_lambda_builds_its_class() {
    assert_eq!(
        common::expect_box_run_against_kotlinc(LIB, MAIN).expect("reference kotlinc is provisioned"),
        "OK"
    );
}

#[test]
fn a_suspend_lambda_in_an_inlined_lambda_is_built_like_the_reference_compiler() {
    let difference = common::method_code_diff_against_kotlinc(
        "InlineSuspendLambda",
        &[("Lib.kt", LIB)],
        MAIN,
        "InlineSuspendLambdaKt",
        "public static final java.lang.String box()",
    )
    .expect("reference kotlinc is provisioned");
    assert_eq!(difference, Ok(()));
}
