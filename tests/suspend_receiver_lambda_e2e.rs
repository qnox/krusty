//! A `suspend R.() -> T` RECEIVER lambda (the coroutine-builder idiom): the checked type folds the
//! receiver into `params[0]`; lowering binds it as the implicit `this` of the `SuspendLambda`
//! body, so bare member reads/writes dispatch on the receiver.

use super::common;

fn run(src: &str) {
    let Some(got) = common::compile_and_run_with_stdlib(src, "MainKt") else {
        panic!("expected the box to compile and run");
    };
    assert_eq!(got, "OK");
}

/// Leaf body (no internal suspension): builder starts the coroutine with the receiver overload.
#[test]
fn suspend_receiver_lambda_member_write() {
    run(r#"
import kotlin.coroutines.*

class Controller {
    var result = ""
}

fun builder(c: suspend Controller.() -> Unit): String {
    val controller = Controller()
    c.startCoroutine(controller, Continuation(EmptyCoroutineContext) {})
    return controller.result
}

fun box(): String {
    return builder {
        result = "OK"
    }
}
"#);
}

/// Member READ + method call through the implicit receiver.
#[test]
fn suspend_receiver_lambda_member_call() {
    run(r#"
import kotlin.coroutines.*

class Controller {
    var log = ""
    fun append(s: String) {
        log += s
    }
}

fun builder(c: suspend Controller.() -> Unit): String {
    val controller = Controller()
    c.startCoroutine(controller, Continuation(EmptyCoroutineContext) {})
    return controller.log
}

fun box(): String = builder {
    append("O")
    append("K")
}
"#);
}

/// Receiver PLUS an explicit value parameter (`suspend R.(A) -> T`): the lambda lowers (receiver
/// bound as `this`, `s` as the value param) — previously this shape skipped the file. The stdlib
/// offers no 3-slot `startCoroutine`, so the builder here only materializes the lambda.
#[test]
fn suspend_receiver_lambda_with_value_param() {
    run(r#"
class Controller {
    var out = ""
}

fun builder(seed: String, c: suspend Controller.(String) -> Unit): String = seed

fun box(): String = builder("OK") { s ->
    out = s
}
"#);
}
