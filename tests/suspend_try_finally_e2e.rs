//! `try`/`catch`/`finally` inside a `suspend fun`. The CPS return-boxing pass (`box_returns`) now
//! descends into a try's body, each catch, and the finally, so a `suspend fun` whose try body does not
//! itself contain a suspension point compiles and runs — the `finally` executes on the normal and the
//! caught-exception paths, and around a state-machine suspension elsewhere in the function.
//!
//! State-machine try/catch/finally is normalized as nested handler responsibilities, and a suspending
//! finally carries a nullable pending exception across its cleanup states before conditionally
//! rethrowing it.

use super::common;

const BUILDER: &str = "import kotlin.coroutines.*\n\
import kotlin.coroutines.intrinsics.*\n\
class Done : Continuation<Unit> {\n\
  override val context: CoroutineContext = EmptyCoroutineContext\n\
  override fun resumeWith(result: Result<Unit>) {}\n\
}\n\
fun builder(c: suspend () -> Unit) { c.startCoroutine(Done()) }\n";

fn run_ok(stem: &str, body: &str) {
    let src = format!("{BUILDER}{body}");
    common::expect_box_ok_with_stdlib(&src, stem);
}

#[test]
fn leaf_suspend_try_finally_with_return_in_try() {
    // A leaf suspend fn (no suspension): `return` inside a try, finally runs. Observable via the
    // captured result being the value returned from the try.
    run_ok(
        "SfLeafRet",
        "suspend fun f(): String { val sb = StringBuilder()\n\
         try { sb.append(\"t\"); return sb.toString() } finally { sb.append(\"X\") } }\n\
         fun box(): String { var r = \"F\"; builder { r = f() }; return if (r == \"t\") \"OK\" else \"F:$r\" }\n",
    );
}

#[test]
fn leaf_suspend_try_catch() {
    // A leaf suspend fn: try/catch, exception caught, boxed return from the catch.
    run_ok(
        "SfLeafCatch",
        "suspend fun f(): Int { return try { throw RuntimeException() } catch (e: Exception) { 42 } }\n\
         fun box(): String { var r = -1; builder { r = f() }; return if (r == 42) \"OK\" else \"F:$r\" }\n",
    );
}

#[test]
fn state_machine_suspend_then_try_finally() {
    // A state-machine suspend fn (a suspension BEFORE the try). The non-suspending try/finally runs
    // after resume; finally's append is observable in the returned string.
    run_ok(
        "SfSmBefore",
        "suspend fun d() {}\n\
         suspend fun f(): String { d(); val sb = StringBuilder()\n\
         try { sb.append(\"t\") } finally { sb.append(\"F\") }\n\
         return sb.toString() }\n\
         fun box(): String { var r = \"F\"; builder { r = f() }; return if (r == \"tF\") \"OK\" else \"F:$r\" }\n",
    );
}

#[test]
fn state_machine_try_finally_then_suspend() {
    // The try/finally runs BEFORE a later suspension point; finally executes, then the fn suspends.
    run_ok(
        "SfSmAfter",
        "suspend fun d() {}\n\
         suspend fun f(): String { val sb = StringBuilder()\n\
         try { sb.append(\"t\") } finally { sb.append(\"F\") }\n\
         d(); return sb.toString() }\n\
         fun box(): String { var r = \"F\"; builder { r = f() }; return if (r == \"tF\") \"OK\" else \"F:$r\" }\n",
    );
}

#[test]
fn leaf_suspend_finally_runs_when_try_returns() {
    // A leaf suspend fn returns from inside a try; the finally still runs — observed via a marker
    // object the finally mutates (the returned value alone can't witness a post-return finally).
    run_ok(
        "SfLeafMark",
        "class Marker { var hit = false }\n\
         suspend fun f(m: Marker): Int { try { return 1 } finally { m.hit = true } }\n\
         fun box(): String {\n\
         val m = Marker()\n\
         var r = -1\n\
         builder { r = f(m) }\n\
         return if (r == 1 && m.hit) \"OK\" else \"F r=$r hit=${m.hit}\" }\n",
    );
}

#[test]
fn suspending_finally_preserves_or_overrides_pending_completion() {
    const MAIN: &str = "import kotlinx.coroutines.runBlocking\n\
        var log = \"\"\n\
        class Caught : RuntimeException()\n\
        class Escaping : RuntimeException()\n\
        class Cleanup : RuntimeException()\n\
        suspend fun body(mode: Int): Int { log += \"B$mode;\"; return when (mode) {\n\
        \x20 0 -> 42\n\
        \x20 1 -> throw Caught()\n\
        \x20 else -> throw Escaping()\n\
        } }\n\
        suspend fun cleanup(fail: Boolean) { log += \"F;\"; if (fail) throw Cleanup() }\n\
        suspend fun run(mode: Int, cleanupFails: Boolean): Int = try {\n\
        \x20 body(mode)\n\
        } catch (e: Caught) {\n\
        \x20 7\n\
        } finally {\n\
        \x20 cleanup(cleanupFails)\n\
        }\n\
        fun box(): String = runBlocking {\n\
        \x20 val normal = run(0, false)\n\
        \x20 val caught = run(1, false)\n\
        \x20 val escaping = try { run(2, false); \"wrong\" } catch (e: Escaping) { \"E\" }\n\
        \x20 val override = try { run(2, true); \"wrong\" } catch (e: Cleanup) { \"C\" }\n\
        \x20 if (normal == 42 && caught == 7 && escaping == \"E\" && override == \"C\" &&\n\
        \x20     log == \"B0;F;B1;F;B2;F;B2;F;\") \"OK\" else \"FAIL:$normal:$caught:$escaping:$override:$log\"\n\
        }\n";
    let jdk = common::jdk_modules();
    let sl = common::stdlib_jar();
    let coro = common::coroutines_jar();
    assert_eq!(
        common::expect_box_run(MAIN, "Main", &[sl, coro, jdk.clone()], Some(jdk.as_path()),),
        "OK"
    );
}

#[test]
fn inline_value_try_with_suspending_branches_binds_after_selection() {
    run_ok(
        "SfInlineValueTry",
        "suspend fun await(value: String): String = value\n\
         inline fun choose(body: () -> String, recover: (RuntimeException) -> String): String =\n\
         try { body() } catch (error: RuntimeException) { recover(error) }\n\
         fun box(): String { var result = \"FAIL\"\n\
         builder { result = choose({ await(\"OK\") }, { await(\"WRONG\") }) }\n\
         return result }\n",
    );
}

#[test]
fn inline_non_local_completion_runs_finally_before_loop_exit() {
    const LIB: &str = "class Controller { var result = \"\" }\n\
        suspend fun lock(owner: Controller) { owner.result += \"L\" }\n\
        fun unlock(owner: Controller) { owner.result += \"U\" }\n\
        public suspend inline fun doInline(owner: Controller, action: () -> Unit): Unit {\n\
        lock(owner); try { return action() } finally { unlock(owner) } }\n";
    const MAIN: &str = "import kotlin.coroutines.*\n\
        fun builder(block: suspend Controller.() -> Unit): String {\n\
        val controller = Controller()\n\
        block.startCoroutine(controller, Continuation(EmptyCoroutineContext) { it.getOrThrow() })\n\
        return controller.result }\n\
        fun box(): String { val value = builder { doInline(this) {} }\n\
        return if (value == \"LU\") \"OK\" else \"FAIL:$value\" }\n";
    common::expect_box_ok_files_with_stdlib(
        &[
            ("SfInlineFinallyLib.kt", LIB),
            ("SfInlineFinallyMain.kt", MAIN),
        ],
        "SfInlineFinallyLoopExit",
    );
}
