//! `kotlin.coroutines` compiler intrinsics — `COROUTINE_SUSPENDED`, `suspendCoroutineUninterceptedOrReturn`,
//! `startCoroutine`. These are `@InlineOnly` stdlib declarations whose stub bodies just `throw`; the
//! reference compiler recognizes them by FQ name (an intrinsics table) and emits dedicated codegen rather
//! than calling/inlining. krusty's splice gate refuses the `throw` body, so without the shared intrinsic
//! registry they resolved to "unresolved". The checker now types them via that compiler table and
//! lowering emits the intrinsic codegen. These compile-only checks pin the
//! resolution+lowering of the leaf shapes (a full coroutine `box()` round-trip additionally needs the
//! companion-object-as-value completion, a separate piece).

use super::common;

use std::path::PathBuf;

fn run(src: &str) -> Option<String> {
    let jh = common::java_home()?;
    let sl = common::stdlib_jar()?;
    let jdk = PathBuf::from(format!("{jh}/lib/modules"));
    common::compile_and_run_box(src, "Main", &[sl], Some(&jdk))
}

fn compiles(src: &str) -> bool {
    let Some(jh) = common::java_home() else {
        return true; // no JDK — skip (treated as pass)
    };
    let Some(sl) = common::stdlib_jar() else {
        return true;
    };
    let jdk = PathBuf::from(format!("{jh}/lib/modules"));
    common::compile_in_process(src, "Coro", &[sl], Some(&jdk)).is_some()
}

#[test]
fn leaf_suspend_unintercepted_or_return_and_coroutine_suspended() {
    const SRC: &str = "import kotlin.coroutines.intrinsics.*\n\
suspend fun suspendForever(): Int = suspendCoroutineUninterceptedOrReturn { COROUTINE_SUSPENDED }\n\
fun box(): String = \"OK\"\n";
    assert!(
        compiles(SRC),
        "leaf coroutine intrinsics should resolve + lower"
    );
}

#[test]
fn start_coroutine_runs_a_suspend_lambda() {
    // `c.startCoroutine(completion)` starts a coroutine: the suspend lambda runs to completion and the
    // completion's `resumeWith` is invoked. Uses a plain `Continuation` completion (not a companion).
    const SRC: &str = "import kotlin.coroutines.*\n\
class Done : Continuation<Unit> {\n\
  override val context: CoroutineContext = EmptyCoroutineContext\n\
  override fun resumeWith(result: Result<Unit>) {}\n\
}\n\
fun builder(c: suspend () -> Unit) { c.startCoroutine(Done()) }\n\
fun box(): String { builder { }; return \"OK\" }\n";
    assert_eq!(
        run(SRC).expect("startCoroutine runs a suspend lambda"),
        "OK"
    );
}

/// A reusable `builder { … }` over a named `Continuation` completion (anonymous-object completions hit a
/// separate property-override gap). Each `box()` declares a LOCAL `var res` the lambda captures and
/// writes — the pattern the coroutine box corpus uses to observe a coroutine's effect.
const BUILDER: &str = "import kotlin.coroutines.*\n\
import kotlin.coroutines.intrinsics.*\n\
class Done : Continuation<Unit> {\n\
  override val context: CoroutineContext = EmptyCoroutineContext\n\
  override fun resumeWith(result: Result<Unit>) {}\n\
}\n\
fun builder(c: suspend () -> Unit) { c.startCoroutine(Done()) }\n";

#[test]
fn suspend_lambda_writes_captured_var_with_state_machine_result() {
    // A suspend lambda assigns the result of a state-machine suspend fn (`simple` calls `dummy` twice)
    // to a captured local `var` (`res = simple()`). Exercises hoisting a suspension out of a captured-var
    // write and the lambda state machine. Round-tripped on the JVM.
    let src = format!(
        "{BUILDER}\
suspend fun dummy() {{}}\n\
suspend fun simple(): String {{ dummy(); dummy(); return \"OK\" }}\n\
fun box(): String {{ var res = \"FAIL\"; builder {{ res = simple() }}; return res }}\n"
    );
    assert_eq!(run(&src).expect("captured-var suspend result runs"), "OK");
}

#[test]
fn suspend_operator_invoke_with_local_receiver() {
    // `g()` is a `suspend operator fun invoke()` on a local receiver — the receiver must be live (spilled)
    // across the suspension; it is constructed before the suspension, not after. Round-tripped on the JVM.
    let src = format!(
        "{BUILDER}\
class GetResult {{ suspend operator fun invoke(): String = \"OK\" }}\n\
fun box(): String {{ var res = \"FAIL\"; builder {{ val g = GetResult(); res = g() }}; return res }}\n"
    );
    assert_eq!(run(&src).expect("suspend operator invoke runs"), "OK");
}

#[test]
fn suspend_coroutine_unintercepted_reads_its_continuation() {
    // `suspendCoroutineUninterceptedOrReturn { c -> c.resume(t); COROUTINE_SUSPENDED }` reads its
    // continuation `c` (bound via the `CurrentContinuation` placeholder, resolved by the CPS pass) and
    // resumes synchronously. Round-tripped on the JVM.
    let src = format!(
        "{BUILDER}\
suspend fun <T> await(t: T): T = suspendCoroutineUninterceptedOrReturn {{ c -> c.resume(t); COROUTINE_SUSPENDED }}\n\
fun box(): String {{ var res = \"FAIL\"; builder {{ res = await(\"OK\") }}; return res }}\n"
    );
    assert_eq!(run(&src).expect("suspendCoroutine reading c runs"), "OK");
}

#[test]
fn coroutine_suspended_as_a_plain_value() {
    const SRC: &str = "import kotlin.coroutines.intrinsics.*\n\
suspend fun f(): Any? = suspendCoroutineUninterceptedOrReturn { val s = COROUTINE_SUSPENDED; s }\n\
fun box(): String = \"OK\"\n";
    assert!(
        compiles(SRC),
        "COROUTINE_SUSPENDED bound to a local should resolve + lower"
    );
}

#[test]
fn string_if_empty_selects_the_charsequence_overload() {
    // Four stdlib `ifEmpty` extensions reach selection as identical `Any`-receiver candidates (their
    // TyParam receivers erase); the JVM descriptor's first parameter must discriminate, or the
    // ARRAY overload's body gets spliced onto a String receiver (`arraylength` → VerifyError).
    let src = "fun box(): String = \"\".ifEmpty { \"OK\" }\n";
    assert_eq!(run(src).expect("String.ifEmpty runs"), "OK");
}

#[test]
fn suspend_fn_type_cast_targets_arity_plus_one_interface() {
    // `suspend () -> Unit` erases to `Function1` (trailing `Continuation`), so an `as` against it
    // must checkcast `Function1`, not `Function0` (KT-66093 shape).
    let src = "fun f(block: (kotlin.coroutines.Continuation<Unit>) -> Any?) { block as (suspend () -> Unit) }\n\
fun box(): String { f {}; return \"OK\" }\n";
    assert_eq!(run(src).expect("suspend fn-type cast runs"), "OK");
}

#[test]
fn inferred_covariant_context_override_gets_supertype_bridge() {
    // `override val context = EmptyCoroutineContext` (inferred type narrows the classpath
    // `Continuation.context: CoroutineContext`): the class needs a `getContext()` BRIDGE returning
    // the supertype's erased type, or interface dispatch throws AbstractMethodError.
    let src = "import kotlin.coroutines.*\n\
class E : Continuation<Any?> {\n\
    override val context = EmptyCoroutineContext\n\
    override fun resumeWith(result: Result<Any?>) {}\n\
}\n\
fun box(): String {\n\
    val c: Continuation<Any?> = E()\n\
    return if (c.context == EmptyCoroutineContext) \"OK\" else \"FAIL\"\n\
}\n";
    assert_eq!(run(src).expect("context bridge dispatches"), "OK");
}

#[test]
fn passed_function_value_splices_into_inline_let() {
    // `t?.let(x)` with a FUNCTION VALUE (not a lambda literal): the verbatim splice binds the param
    // slot to the object and the body's own `Function1.invoke` dispatches on it.
    let src = "fun g(x: (Throwable) -> Unit, t: Throwable?) { t?.let(x) }\n\
fun box(): String {\n\
    var m = \"\"\n\
    g({ m = it.message ?: \"?\" }, RuntimeException(\"OK\"))\n\
    g({ m = \"no\" }, null)\n\
    return m\n\
}\n";
    assert_eq!(run(src).expect("let(fn-value) splices"), "OK");
}

#[test]
fn suspend_fn_entry_has_no_param_null_check() {
    // The state-machine RE-ENTRY call (`foo(null, continuation)`) passes null for every value
    // parameter — kotlinc emits no `checkNotNullParameter` on a suspend fn, so neither must krusty
    // (with the check, the resume NPEs). The conditional tail forces a real re-entry.
    let src = format!(
        "{BUILDER}\
suspend fun sh(): Int = suspendCoroutineUninterceptedOrReturn {{ c -> c.resume(56); COROUTINE_SUSPENDED }}\n\
suspend fun foo(x: Any): Int {{ return if (x == \"56\") sh() else 13 }}\n\
fun box(): String {{ var r = -1; builder {{ r = foo(\"56\") }}; return if (r == 56) \"OK\" else \"fail: $r\" }}\n"
    );
    assert_eq!(run(&src).expect("suspend re-entry runs"), "OK");
}

#[test]
fn unit_suspend_fn_returns_intrinsic_value_not_unit() {
    // `suspend fun …: Unit = suspendCoroutineUninterceptedOrReturn { … COROUTINE_SUSPENDED }` must
    // return the intrinsic's value (the suspension marker), NOT the declared `Unit` — returning
    // `Unit` signals completion while the continuation is pending → double resume.
    let src = format!(
        "{BUILDER}\
class C {{ var v = \"fail\"\n\
  suspend fun put(s: String): Unit = suspendCoroutineUninterceptedOrReturn {{ x -> v = s; x.resume(Unit); COROUTINE_SUSPENDED }} }}\n\
fun box(): String {{ val c = C(); builder {{ c.put(\"OK\") }}; return c.v }}\n"
    );
    assert_eq!(run(&src).expect("unit suspend intrinsic runs"), "OK");
}
