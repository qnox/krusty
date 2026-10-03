//! A local `suspend fun` called with a defaulted suspend lambda omitted
//! (`coroutines/localFunctions/named/defaultArgument.kt`, KT-27449).
//!
//! The omitted call is the function's `$default` edge. That edge is still a suspension of the
//! local function, so the caller's continuation is passed before the mask and marker and the
//! default lambda runs.
use super::common::expect_box_ok_with_stdlib;

#[test]
fn local_suspend_function_omits_its_suspend_lambda_default() {
    let src = r#"
import kotlin.coroutines.*

var result = "Fail"

suspend fun doAction() {
    suspend fun run(
        a: String,
        f: suspend (String) -> Unit = { input -> result = input }
    ) {
        f(a)
    }
    run("OK")
}

fun box(): String {
    suspend { doAction() }.startCoroutine(Continuation(EmptyCoroutineContext) {})
    return result
}
"#;
    expect_box_ok_with_stdlib(
        src,
        "local_suspend_function_omits_its_suspend_lambda_default",
    );
}

#[test]
fn companion_suspend_function_omits_its_default() {
    let src = r#"
import kotlin.coroutines.*

var observed = "Fail"

class Host {
    companion object {
        suspend fun capture(value: String = "OK") {
            observed = value
        }
    }
}

suspend fun exercise() {
    Host.capture()
}

fun box(): String {
    suspend { exercise() }.startCoroutine(Continuation(EmptyCoroutineContext) {})
    return observed
}
"#;
    expect_box_ok_with_stdlib(src, "companion_suspend_function_omits_its_default");
}
