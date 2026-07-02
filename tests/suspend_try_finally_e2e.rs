//! `try`/`catch`/`finally` inside a `suspend fun`. The CPS return-boxing pass (`box_returns`) now
//! descends into a try's body, each catch, and the finally, so a `suspend fun` whose try body does not
//! itself contain a suspension point compiles and runs — the `finally` executes on the normal and the
//! caught-exception paths, and around a state-machine suspension elsewhere in the function.
//!
//! Not yet supported (declined cleanly, never miscompiled): a suspension INSIDE the try body (the
//! finally-across-resume-states shape), and a `catch` clause in a function that also builds a state
//! machine. Those remain separate, larger pieces of work.

mod common;

const BUILDER: &str = "import kotlin.coroutines.*\n\
import kotlin.coroutines.intrinsics.*\n\
class Done : Continuation<Unit> {\n\
  override val context: CoroutineContext = EmptyCoroutineContext\n\
  override fun resumeWith(result: Result<Unit>) {}\n\
}\n\
fun builder(c: suspend () -> Unit) { c.startCoroutine(Done()) }\n";

fn run_ok(stem: &str, body: &str) {
    let Some(java_home) = common::java_home() else {
        eprintln!("skipping suspend_try_finally_e2e: set JAVA_HOME");
        return;
    };
    let Some(stdlib) = common::stdlib_jar() else {
        eprintln!("skipping suspend_try_finally_e2e: no kotlin-stdlib jar found");
        return;
    };
    let jdk = std::path::PathBuf::from(format!("{java_home}/lib/modules"));
    let src = format!("{BUILDER}{body}");
    let Some(out) = common::compile_and_run_box(&src, stem, &[stdlib], Some(&jdk)) else {
        panic!("{stem}: compile/run returned None");
    };
    assert_eq!(out, "OK", "{stem}");
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
