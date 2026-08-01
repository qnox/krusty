//! Suspend + interface delegation: a `class C : I by d` forwarder for a SUSPEND interface method
//! must be a CPS tail-forward (the coroutine pass appends its own `$completion` and threads it
//! into the delegate call, returning the result verbatim) — a plain forwarder drops the
//! continuation (call-arity mismatch at emit) and swallows `COROUTINE_SUSPENDED` (the resume is
//! silently lost). And a `suspend { … }` lambda whose body calls a delegated suspend method must
//! classify as suspending (`ast_body_suspends` sees through the delegation to the interface).

use super::common;

/// Every inline program needs the same terminal `Continuation<Unit>` to start a suspend lambda. Keep
/// the driver in one fixture and append it after the program (Kotlin declarations are order-independent)
/// so the behavior cannot drift between tests. `getOrThrow` deliberately surfaces any uncaught coroutine
/// failure instead of letting a test appear successful after a failed completion.
const UNIT_COMPLETION: &str = r#"
class EC : Continuation<Unit> {
    override val context: CoroutineContext = EmptyCoroutineContext
    override fun resumeWith(result: Result<Unit>) { result.getOrThrow() }
}
"#;

fn expect_suspend_box_ok(source: &str, fixture: &str) {
    let source = format!("{source}\n{UNIT_COMPLETION}");
    common::expect_box_ok_with_stdlib(&source, fixture);
}

/// The `coroutines/tailCallToNothing.kt` shape: a suspend fn that resumes-then-throws through a
/// `suspendCoroutineUninterceptedOrReturn` block, called via a delegation forwarder inside a
/// `suspend { }` lambda's try/catch. Exercises the whole chain: the intrinsic as a real
/// suspension point (the machine's re-entry runs the code after it), the CPS delegation
/// forwarder, and the suspend-lambda state machine.
#[test]
fn suspend_delegation_tail_call_to_nothing() {
    const SRC: &str = r#"import kotlin.coroutines.*
import kotlin.coroutines.intrinsics.*

class Success : RuntimeException()

suspend fun suspendThenThrow(): Nothing {
    suspendCoroutineUninterceptedOrReturn<Unit> {
        it.resume(Unit)
        COROUTINE_SUSPENDED
    }
    throw Success()
}

interface I {
    suspend fun bar(): Nothing
}

class C : I by (object : I {
    override suspend fun bar(): Nothing = suspendThenThrow()
})

fun box(): String {
    var counter = 0
    suspend {
        try { C().bar() } catch (e: Success) { counter++ }
        counter++
    }.startCoroutine(EC())
    return if (counter == 2) "OK" else "counter=$counter"
}
"#;
    expect_suspend_box_ok(SRC, "suspend_deleg_tail_nothing");
}

/// A delegated suspend method that SUSPENDS and then returns a value: the forwarder must deliver
/// the resumed value to the caller (not just propagate the suspension marker).
#[test]
fn suspend_delegation_returns_resumed_value() {
    const SRC: &str = r#"import kotlin.coroutines.*
import kotlin.coroutines.intrinsics.*

interface I {
    suspend fun bar(): Int
}

class Impl : I {
    override suspend fun bar(): Int = suspendCoroutineUninterceptedOrReturn { c ->
        c.resume(42)
        COROUTINE_SUSPENDED
    }
}

class C(val d: I) : I by d

fun box(): String {
    var got = 0
    suspend {
        got = C(Impl()).bar()
    }.startCoroutine(EC())
    return if (got == 42) "OK" else "got=$got"
}
"#;
    expect_suspend_box_ok(SRC, "suspend_deleg_resumed_value");
}

/// An intrinsic block inside a state machine is not merely a call-shaped marker: it can park the
/// machine wrapper for a genuinely LATER resume. Exercise two such points in one activation so the
/// first resume must run the intervening statement and park again, while the second must restore both
/// values and finish. This guards the explicit IR distinction between callable suspend nodes (which
/// receive an appended continuation argument) and already-inlined intrinsic suspension points.
#[test]
fn intrinsic_points_resume_asynchronously_twice() {
    const SRC: &str = r#"import kotlin.coroutines.*
import kotlin.coroutines.intrinsics.*

var parked: Continuation<Int>? = null
var stage = 0

suspend fun compute(): Int {
    val a = suspendCoroutineUninterceptedOrReturn<Int> { c ->
        parked = c
        COROUTINE_SUSPENDED
    }
    stage = 1
    val b = suspendCoroutineUninterceptedOrReturn<Int> { c ->
        parked = c
        COROUTINE_SUSPENDED
    }
    stage = 2
    return a + b
}

fun box(): String {
    var got = -1
    suspend { got = compute() }.startCoroutine(EC())
    if (stage != 0 || got != -1) return "ran before first resume: stage=$stage got=$got"
    parked!!.resume(19)
    if (stage != 1 || got != -1) return "first resume: stage=$stage got=$got"
    parked!!.resume(23)
    return if (stage == 2 && got == 42) "OK" else "second resume: stage=$stage got=$got"
}
"#;
    expect_suspend_box_ok(SRC, "intrinsic_async_twice");
}

/// A continuation-reading intrinsic can also be a conditional TAIL return. That path forwards the
/// incoming continuation directly and needs no local resume state; forcing the entire private function
/// through a state machine both loses the tail shape and requires cross-class private re-entry machinery.
/// Keep the corpus regression explicit so adding non-tail intrinsic points cannot demote this supported
/// branch form back to a backend skip.
#[test]
fn private_suspend_conditional_intrinsic_tail_stays_supported() {
    const SRC: &str = r#"import kotlin.coroutines.*
import kotlin.coroutines.intrinsics.*

var chooseIntrinsic = true

private suspend fun select(): String {
    if (chooseIntrinsic) {
        return suspendCoroutineUninterceptedOrReturn<String> { c ->
            c.resume("OK")
            COROUTINE_SUSPENDED
        }
    }
    return "fail"
}

fun box(): String {
    var result = ""
    suspend { result = select() }.startCoroutine(EC())
    return result
}
"#;
    expect_suspend_box_ok(SRC, "private_suspend_conditional_tail");
}

/// A suspend lambda whose only suspension is a MEMBER call exposed through interface delegation:
/// `ast_body_suspends` must classify the body as suspending (the class's own method table doesn't
/// list the delegated method), so it gets a state machine instead of a broken leaf.
#[test]
fn suspend_lambda_member_call_through_delegation_suspends() {
    const SRC: &str = r#"import kotlin.coroutines.*
import kotlin.coroutines.intrinsics.*

interface I {
    suspend fun ping(): Unit
}

class Impl : I {
    override suspend fun ping() {
        suspendCoroutineUninterceptedOrReturn<Unit> { c ->
            c.resume(Unit)
            COROUTINE_SUSPENDED
        }
    }
}

class C(val d: I) : I by d

fun box(): String {
    var hit = false
    suspend {
        C(Impl()).ping()
        hit = true
    }.startCoroutine(EC())
    return if (hit) "OK" else "fail"
}
"#;
    expect_suspend_box_ok(SRC, "suspend_lambda_deleg_member");
}

/// A suspend DEFAULT interface method gets a state machine whose continuation wrapper re-invokes
/// it: the re-entry call must be `invokeinterface`, not `invokevirtual` (an interface methodref
/// fails linkage with `IncompatibleClassChangeError` — coroutines/suspendDefaultImpl).
#[test]
fn suspend_default_interface_method_resumes() {
    const SRC: &str = r#"import kotlin.coroutines.*
import kotlin.coroutines.intrinsics.*

interface ResumableDefault {
    suspend fun toInt(): Int = suspendCoroutineUninterceptedOrReturn { x ->
        x.resume(56)
        COROUTINE_SUSPENDED
    }
}

class DefaultCarrier : ResumableDefault

fun box(): String {
    var result = -1
    suspend {
        result = DefaultCarrier().toInt()
    }.startCoroutine(EC())
    return if (result == 56) "OK" else "fail: $result"
}
"#;
    expect_suspend_box_ok(SRC, "suspend_default_iface_method");
}

/// The intrinsic call must never type as `Nothing` — even in a `(): Nothing` fn without an
/// explicit type argument (kotlinc infers the free `T` as `Unit`). A `Nothing` type made
/// ir_lower drop the statements after the intrinsic as unreachable, so the resume-then-throw
/// protocol lost the throw.
#[test]
fn intrinsic_call_never_types_as_nothing() {
    const SRC: &str = r#"import kotlin.coroutines.*
import kotlin.coroutines.intrinsics.*

class Success : RuntimeException()

suspend fun suspendThenThrow(): Nothing {
    suspendCoroutineUninterceptedOrReturn {
        it.resume(Unit)
        COROUTINE_SUSPENDED
    }
    throw Success()
}

fun box(): String {
    var counter = 0
    suspend {
        try { suspendThenThrow() } catch (e: Success) { counter++ }
        counter++
    }.startCoroutine(EC())
    return if (counter == 2) "OK" else "counter=$counter"
}
"#;
    expect_suspend_box_ok(SRC, "intrinsic_never_nothing");
}
