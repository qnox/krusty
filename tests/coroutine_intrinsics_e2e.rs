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
    let jh = common::java_home();
    let sl = common::stdlib_jar();
    let jdk = PathBuf::from(format!("{jh}/lib/modules"));
    common::compile_and_run_box(src, "Main", &[sl], Some(&jdk))
}

fn compiles(src: &str) -> bool {
    let jh = common::java_home();
    let sl = common::stdlib_jar();
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
fn suspend_operator_get_convention_is_a_suspension_point() {
    // `b[1]` is an `Expr::Index`, never an `Expr::Call` — the coroutine classification scan used to
    // miss it, so a lambda whose ONLY suspension is a convention call was typed non-suspend and the
    // whole file died at emit. The selected `get` operator is a suspension point like any other.
    let src = format!(
        "{BUILDER}\
class Box(val v: Int) {{ suspend operator fun get(i: Int): Int = v + i }}\n\
fun box(): String {{ var r = 0; builder {{ val b = Box(1); r = b[1] }}; return if (r == 2) \"OK\" else \"fail: $r\" }}\n"
    );
    assert_eq!(run(&src).expect("suspend operator get runs"), "OK");
}

/// A public `suspend inline` member still has a callable CPS entry. Convention calls inside a generated
/// suspend-lambda state machine use that entry just like ordinary suspend members; only a selected
/// `MustInline` target, which has no callable entry, belongs at the splice-safety gate.
#[test]
fn suspend_inline_operator_plus_in_suspend_lambda_calls_cps_entry() {
    let src = format!(
        "{BUILDER}\
class Box(val v: Int) {{ suspend inline operator fun plus(i: Int): Box = Box(v + i) }}\n\
fun box(): String {{ var r = 0; builder {{ var b = Box(1); b += 2; r = b.v }}; return if (r == 3) \"OK\" else \"fail: $r\" }}\n"
    );
    assert_eq!(run(&src).expect("suspend-inline operator plus runs"), "OK");
}

/// The statement-keyed half of the same rule. `plusAssign` is retained as a specialized
/// `CompoundAssignmentTarget`, and its exact selected suspend target must reach the state machine.
#[test]
fn suspend_inline_operator_plus_assign_in_suspend_lambda_calls_cps_entry() {
    let src = format!(
        "{BUILDER}\
class Box(var v: Int) {{ suspend inline operator fun plusAssign(i: Int) {{ v += i }} }}\n\
fun box(): String {{ var r = 0; builder {{ val b = Box(1); b += 2; r = b.v }}; return if (r == 3) \"OK\" else \"fail: $r\" }}\n"
    );
    assert_eq!(
        run(&src).expect("suspend-inline operator plusAssign runs"),
        "OK"
    );
}

/// Relational syntax records the same exact selected target as every other operator and calls its
/// public CPS entry from the generated state machine.
#[test]
fn suspend_inline_operator_compare_to_in_suspend_lambda_calls_cps_entry() {
    let src = format!(
        "{BUILDER}\
class Box(val v: Int) {{ suspend inline operator fun compareTo(other: Box): Int = v - other.v }}\n\
fun box(): String {{ var r = false; builder {{ r = Box(1) < Box(2) }}; return if (r) \"OK\" else \"fail\" }}\n"
    );
    assert_eq!(
        run(&src).expect("suspend-inline operator compareTo runs"),
        "OK"
    );
}

#[test]
fn suspend_operator_plus_convention_is_a_suspension_point() {
    // `b += 2` against a value-returning `plus` desugars to `b = b.plus(2)` — a synthetic
    // `Expr::Binary`, so the selected operator is recorded against that EXPRESSION.
    let src = format!(
        "{BUILDER}\
class Box(val v: Int) {{ suspend operator fun plus(i: Int): Box = Box(v + i) }}\n\
fun box(): String {{ var r = 0; builder {{ var b = Box(1); b += 2; r = b.v }}; return if (r == 3) \"OK\" else \"fail: $r\" }}\n"
    );
    assert_eq!(run(&src).expect("suspend operator plus runs"), "OK");
}

#[test]
fn suspend_operator_plus_assign_convention_is_a_suspension_point() {
    // The in-place spelling. A `Unit`-returning `plusAssign` mutates the receiver, so the checker
    // records it as a `CompoundAssignmentTarget` against the STATEMENT — the one convention key
    // neither the call scan nor the expression tables reach. Distinct from the `plus` case above:
    // only this shape exercises the specialized target's retained callable capabilities.
    let src = format!(
        "{BUILDER}\
class Box(var v: Int) {{ suspend operator fun plusAssign(i: Int) {{ v += i }} }}\n\
fun box(): String {{ var r = 0; builder {{ val b = Box(1); b += 2; r = b.v }}; return if (r == 3) \"OK\" else \"fail: $r\" }}\n"
    );
    assert_eq!(run(&src).expect("suspend operator plusAssign runs"), "OK");
}

#[test]
fn suspend_operator_compare_to_convention_is_a_suspension_point() {
    // A SOURCE-class member `compareTo` driving `<` records the same exact operator target that
    // lowering emits. The suspend classifier consumes its capabilities without a source-only
    // hierarchy/name fallback.
    let src = format!(
        "{BUILDER}\
class Box(val v: Int) {{ suspend operator fun compareTo(o: Box): Int = v - o.v }}\n\
fun box(): String {{ var r = 0; builder {{ val less = Box(1) < Box(2); r = if (less) 7 else 9 }}; return if (r == 7) \"OK\" else \"fail: $r\" }}\n"
    );
    assert_eq!(run(&src).expect("suspend operator compareTo runs"), "OK");
}

/// `h?.get()` is an `Expr::SafeCall`, not an `Expr::Call` — the same "a call that is not spelled as
/// one" blind spot as the operator conventions, and it reached the identical failure: the lambda was
/// classified non-suspend and the file died at emit with NO labelled reason, while the `h!!.get()`
/// spelling of the very same call compiled and ran.
///
/// The state-machine pass still declines a suspension on a safe-call's short-circuiting branch, so
/// this shape does not run yet — but it must now decline at its own named boundary instead of
/// silently falling off the end of emission. Pinning the reason is what distinguishes "we know we
/// can't do this" from "we never noticed there was a suspension here".
#[test]
fn suspend_call_behind_a_safe_call_is_seen_as_a_suspension() {
    let src = format!(
        "{BUILDER}\
class Holder {{ suspend fun get(): String = \"OK\" }}\n\
fun box(): String {{ var r: String? = \"FAIL\"; val h: Holder? = Holder(); builder {{ r = h?.get() }}; return r ?: \"NULL\" }}\n"
    );
    common::assert_inline_source_backend_bail(&src, krusty::jvm::backend::SkipReason::Suspend);
}

/// The OTHER boundary the convention entries above are pinned against, in its bare form: a plain
/// `suspend fun` call, ONE file, no operator convention and no extension anywhere. It reaches the
/// same labelled `SkipReason::Suspend`, which is what forbids re-attributing the cross-file
/// comparison skip in `cross_file_inline_call_e2e` to the convention (as it once was, to a
/// suspending `RefSet`). kotlinc answers `7`.
#[test]
fn suspend_in_an_if_expression_into_a_captured_var_skips_without_a_convention() {
    let src = format!(
        "{BUILDER}\
suspend fun less(): Boolean = true\n\
fun box(): String {{ var r = 0; builder {{ r = if (less()) 7 else 9 }}; return if (r == 7) \"OK\" else \"fail: $r\" }}\n"
    );
    common::assert_inline_source_backend_bail(&src, krusty::jvm::backend::SkipReason::Suspend);
}

/// Half one of the disambiguation: the SAME suspending condition, the same captured `var` target,
/// but the `if` is a STATEMENT rather than an expression — it compiles and runs. So "a suspension in
/// an `if` condition" does not on its own name the boundary above.
#[test]
fn suspend_in_an_if_statement_condition_into_a_captured_var_runs() {
    let src = format!(
        "{BUILDER}\
suspend fun less(): Boolean = true\n\
fun box(): String {{ var r = 0; builder {{ if (less()) {{ r = 7 }} else {{ r = 9 }} }}; return if (r == 7) \"OK\" else \"fail: $r\" }}\n"
    );
    assert_eq!(
        run(&src).expect("suspending if-STATEMENT condition runs"),
        "OK"
    );
}

/// Half two: the same suspending condition in a genuine if-EXPRESSION, but its value lands in a
/// LOCAL instead of a captured `var` — also compiles and runs. Only the CONJUNCTION of the two
/// halves declines, which is what the SPEC entry has to say for the boundary to be reproducible.
#[test]
fn suspend_in_an_if_expression_into_a_local_runs() {
    let src = format!(
        "{BUILDER}\
suspend fun less(): Boolean = true\n\
fun box(): String {{ var r = 0; builder {{ val x = if (less()) 7 else 9; r = x }}; return if (r == 7) \"OK\" else \"fail: $r\" }}\n"
    );
    assert_eq!(
        run(&src).expect("suspending if-EXPRESSION into a local runs"),
        "OK"
    );
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
