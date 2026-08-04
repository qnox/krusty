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

/// A parameter slot (here the receiver) PLUS a CAPTURED enclosing local. Both are modeled by the same
/// `SuspendLambda` fields — captures are stored by the constructor, parameter slots by `create`/`invoke` —
/// so the combination lowers; it used to bail (skip the file) as a leaf-only scope limit.
#[test]
fn suspend_receiver_lambda_captures_and_receiver() {
    run(r#"
import kotlin.coroutines.*

class Scope(val budget: Int)

fun withScope(body: suspend Scope.() -> Unit) {
    body.startCoroutine(Scope(7), Continuation(EmptyCoroutineContext) {})
}

fun box(): String {
    var seen = 0
    withScope { seen += budget }
    return if (seen == 7) "OK" else "FAIL $seen"
}
"#);
}

/// The same combination with an ordinary VALUE parameter rather than a receiver.
#[test]
fn suspend_value_param_lambda_captures() {
    run(r#"
import kotlin.coroutines.*

fun apply(x: Int, body: suspend (Int) -> Unit) {
    val started: suspend () -> Unit = { body(x) }
    started.startCoroutine(Continuation(EmptyCoroutineContext) {})
}

fun box(): String {
    var seen = 0
    apply(7) { v -> seen += v }
    return if (seen == 7) "OK" else "FAIL $seen"
}
"#);
}

/// A suspend-lambda safety gate must inspect the SELECTED callable, not every declaration sharing its
/// source name. The body selects the ordinary top-level `record`; the unrelated class member is
/// `suspend inline` only to make a name-wide scan maximally misleading. The old scan rejected the whole
/// file before lowering, even though neither overload selection nor emitted bytecode could reach the
/// member. Exercise the ordinary call both directly and inside the suspend lambda: the file-level
/// caller-context gate and the lambda state-machine classifier must share exact target semantics. This
/// regression deliberately uses neutral names so diagnostics and compiler traces never need to expose
/// an application class name to classify the call.
#[test]
fn suspend_lambda_ignores_unselected_same_named_suspend_inline_member() {
    run(r#"
import kotlin.coroutines.*

var observed = ""

class Unrelated {
    suspend inline fun record() { observed = "wrong" }
}

fun record() { observed = "OK" }

fun start(body: suspend () -> Unit) {
    body.startCoroutine(Continuation(EmptyCoroutineContext) {})
}

fun box(): String {
    record()
    start { record() }
    return observed
}
"#);
}

/// The complementary safety boundary: when the selected target itself is `suspend inline`, lowering
/// must still decline before it emits a direct call from generated state-machine code. Assert the
/// stable capability-based reason exactly; putting the source function's name in this diagnostic would
/// leak application identifiers and couple tests to a real declaration spelling.
#[test]
fn suspend_lambda_rejects_selected_suspend_inline_without_name_leakage() {
    let stdlib = common::stdlib_jar();
    let jdk = common::jdk_modules();
    let source = r#"
import kotlin.coroutines.*

suspend inline fun selectedOperation() {}

fun start(body: suspend () -> Unit) {
    body.startCoroutine(Continuation(EmptyCoroutineContext) {})
}

fun box(): String {
    start { selectedOperation() }
    return "unreachable"
}
"#;
    let outcome =
        common::backend_outcome_in_process(source, "SelectedSuspendInline", &[stdlib], Some(&jdk))
            .expect("the front end must accept the regression source");
    assert_eq!(
        outcome,
        common::BackendOutcome::LowerBail("gate:suspend-inline-call-in-suspend-lambda".to_string())
    );
}

/// A `Unit` tail that leaves NOTHING on the operand stack must materialize the `Unit` singleton — a
/// call to a `Unit` function VALUE, after a suspension. Storing the tail into the machine's result temp
/// instead read from an empty stack (`VerifyError: Operand stack underflow`).
#[test]
fn suspend_receiver_lambda_unit_tail_invokes_function_value() {
    run(r#"
import kotlin.coroutines.*

class Scope(val budget: Int)

suspend fun work(): Int = 5

var out = ""

fun withScope(body: suspend Scope.() -> Unit) {
    body.startCoroutine(Scope(7), Continuation(EmptyCoroutineContext) {})
}

fun box(): String {
    val f: () -> Unit = { out += "!" }
    withScope {
        val a = work()
        out = "" + a + budget
        f()
    }
    return if (out == "57!") "OK" else "FAIL $out"
}
"#);
}

/// The same rule for a `Unit` `try` tail, with a sub-int local live across the suspension (the shape the
/// removed receiver-lambda spill bail used to skip).
#[test]
fn suspend_receiver_lambda_unit_try_tail() {
    run(r#"
import kotlin.coroutines.*

class Scope(val budget: Int)

suspend fun work(): Int = 5

var out = ""

fun withScope(body: suspend Scope.() -> Unit) {
    body.startCoroutine(Scope(7), Continuation(EmptyCoroutineContext) {})
}

fun box(): String {
    withScope {
        val flag = budget > 3
        val a = work()
        try { out = "" + flag + a } catch (e: Exception) { out = "x" }
    }
    return if (out == "true5") "OK" else "FAIL $out"
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
