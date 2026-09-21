//! A suspension inside a lambda passed to a CLASSPATH `inline` function.
//!
//! Once the call is spliced, that lambda's body runs in the enclosing method's own frame, so the
//! suspension belongs to the enclosing coroutine. The IR machine cannot see it — it stops at every
//! lambda — so the enclosing function was classified as having no suspension point at all and the
//! spliced call was emitted with no continuation to pass:
//!
//! ```text
//! error: krusty: JVM backend inline error: call arity mismatch
//! ```
//!
//! The machine for these is built during emission instead, where the spliced body's own locals
//! exist and can be spilled. See `docs/JVM_INLINE_BEFORE_CPS.md`.

use super::common;

/// Compile `main` against a reference-compiled inline library, with coroutines on the classpath.
fn run(tag: &str, main: &str) -> Option<String> {
    let jdk = common::jdk_modules();
    let libout = common::compile_lib_ref(tag, LIB)?;
    let cp = [
        libout,
        common::stdlib_jar(),
        common::coroutines_jar(),
        jdk.clone(),
    ];
    Some(common::expect_box_run(
        main,
        "Main",
        &cp,
        Some(jdk.as_path()),
    ))
}

/// A classpath inline function with a loop and accumulator locals — none of which exist until it
/// has been spliced, which is exactly what the machine has to spill.
const LIB: &str = r#"
inline fun <R> attempt(f: () -> R): R? = try { f() } catch (e: Throwable) { null }

inline fun twice(x: Int, f: (Int) -> Int): Int {
    var acc = 0
    var i = 0
    while (i < 2) {
        val r = f(x + i)
        acc = acc + r
        i = i + 1
    }
    return acc
}
"#;

#[test]
fn a_suspension_inside_a_spliced_inline_lambda_runs() {
    const MAIN: &str = r#"
        import kotlinx.coroutines.runBlocking
        suspend fun one(v: Int): Int = v + 1
        suspend fun many(v: Int): Int = twice(v) { one(it) }
        fun box(): String = runBlocking {
            val n = many(10)
            if (n == 23) "OK" else "FAIL: " + n
        }
    "#;
    let Some(output) = run("suspend_spliced_inline", MAIN) else {
        return; // toolchain not provisioned
    };
    assert_eq!(output, "OK");
}

/// Two suspensions in one spliced body: each needs its own state, and the second must resume with
/// the first one's result already restored.
#[test]
fn two_suspensions_in_one_spliced_body_run() {
    const MAIN: &str = r#"
        import kotlinx.coroutines.runBlocking
        suspend fun one(v: Int): Int = v + 1
        suspend fun pair(v: Int): Int = twice(v) { one(it) + one(it) }
        fun box(): String = runBlocking {
            val n = pair(10)
            if (n == 46) "OK" else "FAIL: " + n
        }
    "#;
    let Some(output) = run("suspend_spliced_two", MAIN) else {
        return;
    };
    assert_eq!(output, "OK");
}

/// A reference-typed local live across the suspension: spilled into the `L$0` family and narrowed
/// back on resume, where a primitive spill is not.
#[test]
fn a_reference_local_survives_a_spliced_suspension() {
    const MAIN: &str = r#"
        import kotlinx.coroutines.runBlocking
        suspend fun one(v: Int): Int = v + 1
        suspend fun tagged(tag: String, v: Int): String {
            val n = twice(v) { one(it) }
            return tag + n
        }
        fun box(): String = runBlocking {
            val s = tagged("n=", 10)
            if (s == "n=23") "OK" else "FAIL: " + s
        }
    "#;
    let Some(output) = run("suspend_spliced_ref", MAIN) else {
        return;
    };
    assert_eq!(output, "OK");
}

/// The suspension sits in one arm of a conditional inside the spliced body, so only some paths
/// reach it and the resume re-enters mid-branch.
#[test]
fn a_conditional_suspension_in_a_spliced_body_runs() {
    const MAIN: &str = r#"
        import kotlinx.coroutines.runBlocking
        suspend fun one(v: Int): Int = v + 1
        suspend fun odds(v: Int): Int = twice(v) { if (it % 2 == 0) one(it) else it }
        fun box(): String = runBlocking {
            val n = odds(10)
            if (n == 22) "OK" else "FAIL: " + n
        }
    "#;
    let Some(output) = run("suspend_spliced_cond", MAIN) else {
        return;
    };
    assert_eq!(output, "OK");
}

/// A two-slot local (`Long`) live across the suspension: one verification entry, two slots.
#[test]
fn a_long_local_survives_a_spliced_suspension() {
    const MAIN: &str = r#"
        import kotlinx.coroutines.runBlocking
        suspend fun one(v: Int): Int = v + 1
        suspend fun wide(v: Int): Long {
            val base = 100L
            val n = twice(v) { one(it) }
            return base + n
        }
        fun box(): String = runBlocking {
            val n = wide(10)
            if (n == 123L) "OK" else "FAIL: " + n
        }
    "#;
    let Some(output) = run("suspend_spliced_long", MAIN) else {
        return;
    };
    assert_eq!(output, "OK");
}

/// The same, through a stdlib `inline fun` rather than a fixture one: `run`'s body is spliced, so
/// the suspension inside it belongs to this frame.
#[test]
fn a_suspension_inside_a_spliced_stdlib_run_lambda_runs() {
    const MAIN: &str = r#"
        import kotlinx.coroutines.runBlocking
        suspend fun one(v: Int): Int = v + 1
        suspend fun viaRun(v: Int): Int = run { one(v) }
        fun box(): String = runBlocking {
            val n = viaRun(10)
            if (n == 11) "OK" else "FAIL: " + n
        }
    "#;
    let Some(output) = run("probe_run", MAIN) else {
        return;
    };
    assert_eq!(output, "OK");
}

/// `let` passes the receiver as the lambda's argument, so the spliced body opens with a parameter
/// slot this emitter allocated rather than one any frame describes.
#[test]
fn a_suspension_inside_a_spliced_stdlib_let_lambda_runs() {
    const MAIN: &str = r#"
        import kotlinx.coroutines.runBlocking
        suspend fun one(v: Int): Int = v + 1
        suspend fun viaLet(v: Int): Int = v.let { one(it) }
        fun box(): String = runBlocking {
            val n = viaLet(10)
            if (n == 11) "OK" else "FAIL: " + n
        }
    "#;
    let Some(output) = run("probe_let", MAIN) else {
        return;
    };
    assert_eq!(output, "OK");
}

/// A suspension inside a spliced LOOP body, resumed once per iteration, with a captured
/// accumulator live across it.
#[test]
fn a_suspension_inside_a_spliced_stdlib_repeat_lambda_runs() {
    const MAIN: &str = r#"
        import kotlinx.coroutines.runBlocking
        suspend fun one(v: Int): Int = v + 1
        suspend fun viaRepeat(v: Int): Int {
            var acc = 0
            repeat(3) { acc = acc + one(v) }
            return acc
        }
        fun box(): String = runBlocking {
            val n = viaRepeat(10)
            if (n == 33) "OK" else "FAIL: " + n
        }
    "#;
    let Some(output) = run("probe_repeat", MAIN) else {
        return;
    };
    assert_eq!(output, "OK");
}

/// An instance method's machine: the continuation keeps the receiver, because re-entering the
/// method needs one.
#[test]
fn a_suspension_inside_a_spliced_lambda_of_a_member_runs() {
    const MAIN: &str = r#"
        import kotlinx.coroutines.runBlocking
        suspend fun one(v: Int): Int = v + 1
        class Holder(val base: Int) {
            suspend fun total(): Int = twice(base) { one(it) }
        }
        fun box(): String = runBlocking {
            val n = Holder(10).total()
            if (n == 23) "OK" else "FAIL: " + n
        }
    "#;
    let Some(output) = run("suspend_spliced_member", MAIN) else {
        return;
    };
    assert_eq!(output, "OK");
}

/// The lambda's body is spliced INTO A PROTECTED REGION: the inline function wraps it in
/// `try`/`catch`. The suspension leaves the method from inside that region and the resume re-enters
/// it. (`runCatching { … }` around a suspending call is this shape.)
#[test]
fn a_suspension_inside_a_spliced_try_region_runs() {
    const MAIN: &str = r#"
        import kotlinx.coroutines.runBlocking
        suspend fun one(v: Int): Int = v + 1
        suspend fun guarded(v: Int): Int = attempt { one(v) } ?: -1
        fun box(): String = runBlocking {
            val n = guarded(10)
            if (n == 11) "OK" else "FAIL: " + n
        }
    "#;
    let Some(output) = run("suspend_spliced_try", MAIN) else {
        return;
    };
    assert_eq!(output, "OK");
}

#[test]
fn probe_stdlib_run_catching() {
    const MAIN: &str = r#"
        import kotlinx.coroutines.runBlocking
        suspend fun one(v: Int): Int = v + 1
        suspend fun guardedStdlib(v: Int): Int = runCatching { one(v) }.getOrDefault(-1)
        fun box(): String = runBlocking {
            val n = guardedStdlib(10)
            if (n == 11) "OK" else "FAIL: " + n
        }
    "#;
    let Some(output) = run("probe_run_catching", MAIN) else {
        return;
    };
    assert_eq!(output, "OK");
}

/// A PRIVATE member's machine: the continuation cannot call the method directly, so it re-enters
/// through the synthetic `access$<name>` static, as kotlinc's does.
#[test]
fn a_suspension_inside_a_spliced_lambda_of_a_private_member_runs() {
    const MAIN: &str = r#"
        import kotlinx.coroutines.runBlocking
        suspend fun one(v: Int): Int = v + 1
        class Holder(val base: Int) {
            private suspend fun step(): Int = twice(base) { one(it) }
            suspend fun total(): Int = step() + 1
        }
        fun box(): String = runBlocking {
            val n = Holder(10).total()
            if (n == 24) "OK" else "FAIL: " + n
        }
    "#;
    let Some(output) = run("suspend_spliced_private", MAIN) else {
        return;
    };
    assert_eq!(output, "OK");
}

/// A MIXED function: one suspension of its own, which the IR machine can see, and another inside a
/// spliced lambda, which it cannot. One method has one dispatch, so the emit-time machine takes
/// both.
#[test]
fn a_function_that_suspends_both_in_and_outside_a_spliced_body_runs() {
    const MAIN: &str = r#"
        import kotlinx.coroutines.runBlocking
        suspend fun one(v: Int): Int = v + 1
        suspend fun mixed(v: Int): Int {
            val a = one(v)
            val b = twice(a) { one(it) }
            return a + b
        }
        fun box(): String = runBlocking {
            val n = mixed(10)
            if (n == 36) "OK" else "FAIL: " + n
        }
    "#;
    let Some(output) = run("suspend_spliced_mixed", MAIN) else {
        return;
    };
    assert_eq!(output, "OK");
}
