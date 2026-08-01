//! Suspend + interface delegation: a `class C : I by d` forwarder for a SUSPEND interface method
//! must be a CPS tail-forward (the coroutine pass appends its own `$completion` and threads it
//! into the delegate call, returning the result verbatim) — a plain forwarder drops the
//! continuation (call-arity mismatch at emit) and swallows `COROUTINE_SUSPENDED` (the resume is
//! silently lost). And a `suspend { … }` lambda whose body calls a delegated suspend method must
//! classify as suspending (`ast_body_suspends` sees through the delegation to the interface).

use super::common;

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

class EC : Continuation<Unit> {
    override val context: CoroutineContext = EmptyCoroutineContext
    override fun resumeWith(result: Result<Unit>) {}
}

fun box(): String {
    var counter = 0
    suspend {
        try { C().bar() } catch (e: Success) { counter++ }
        counter++
    }.startCoroutine(EC())
    return if (counter == 2) "OK" else "counter=$counter"
}
"#;
    common::expect_box_ok_with_stdlib(SRC, "suspend_deleg_tail_nothing");
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

class EC : Continuation<Unit> {
    override val context: CoroutineContext = EmptyCoroutineContext
    override fun resumeWith(result: Result<Unit>) {}
}

fun box(): String {
    var got = 0
    suspend {
        got = C(Impl()).bar()
    }.startCoroutine(EC())
    return if (got == 42) "OK" else "got=$got"
}
"#;
    common::expect_box_ok_with_stdlib(SRC, "suspend_deleg_resumed_value");
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

class EC : Continuation<Unit> {
    override val context: CoroutineContext = EmptyCoroutineContext
    override fun resumeWith(result: Result<Unit>) {}
}

fun box(): String {
    var hit = false
    suspend {
        C(Impl()).ping()
        hit = true
    }.startCoroutine(EC())
    return if (hit) "OK" else "fail"
}
"#;
    common::expect_box_ok_with_stdlib(SRC, "suspend_lambda_deleg_member");
}

/// A suspend DEFAULT interface method gets a state machine whose continuation wrapper re-invokes
/// it: the re-entry call must be `invokeinterface`, not `invokevirtual` (an interface methodref
/// fails linkage with `IncompatibleClassChangeError` — coroutines/suspendDefaultImpl).
#[test]
fn suspend_default_interface_method_resumes() {
    const SRC: &str = r#"import kotlin.coroutines.*
import kotlin.coroutines.intrinsics.*

interface TestInterface {
    suspend fun toInt(): Int = suspendCoroutineUninterceptedOrReturn { x ->
        x.resume(56)
        COROUTINE_SUSPENDED
    }
}

class TestClass2 : TestInterface

class EC : Continuation<Unit> {
    override val context: CoroutineContext = EmptyCoroutineContext
    override fun resumeWith(result: Result<Unit>) { result.getOrThrow() }
}

fun box(): String {
    var result = -1
    suspend {
        result = TestClass2().toInt()
    }.startCoroutine(EC())
    return if (result == 56) "OK" else "fail: $result"
}
"#;
    common::expect_box_ok_with_stdlib(SRC, "suspend_default_iface_method");
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

class EC : Continuation<Unit> {
    override val context: CoroutineContext = EmptyCoroutineContext
    override fun resumeWith(result: Result<Unit>) {}
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
    common::expect_box_ok_with_stdlib(SRC, "intrinsic_never_nothing");
}
