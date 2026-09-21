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

inline fun holdRef(f: () -> Int): Int {
    val sb = StringBuilder("p")
    sb.append(f())
    return sb.length
}

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
        suspend fun one(v: Int): Int { kotlinx.coroutines.yield(); return v + 1 }
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
        suspend fun one(v: Int): Int { kotlinx.coroutines.yield(); return v + 1 }
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
        suspend fun one(v: Int): Int { kotlinx.coroutines.yield(); return v + 1 }
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
        suspend fun one(v: Int): Int { kotlinx.coroutines.yield(); return v + 1 }
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
        suspend fun one(v: Int): Int { kotlinx.coroutines.yield(); return v + 1 }
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
        suspend fun one(v: Int): Int { kotlinx.coroutines.yield(); return v + 1 }
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
        suspend fun one(v: Int): Int { kotlinx.coroutines.yield(); return v + 1 }
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
        suspend fun one(v: Int): Int { kotlinx.coroutines.yield(); return v + 1 }
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
        suspend fun one(v: Int): Int { kotlinx.coroutines.yield(); return v + 1 }
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
        suspend fun one(v: Int): Int { kotlinx.coroutines.yield(); return v + 1 }
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

/// The corpus shape: a stdlib `runCatching` around a suspending call.
#[test]
fn a_suspension_inside_stdlib_run_catching_runs() {
    const MAIN: &str = r#"
        import kotlinx.coroutines.runBlocking
        suspend fun one(v: Int): Int { kotlinx.coroutines.yield(); return v + 1 }
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
        suspend fun one(v: Int): Int { kotlinx.coroutines.yield(); return v + 1 }
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
        suspend fun one(v: Int): Int { kotlinx.coroutines.yield(); return v + 1 }
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

/// A mixed function whose suspensions sit inside a `try`/`finally`. The handler's frame claims the
/// locals the protected code assigned, so every state must restore them BEFORE re-entering the
/// region — which is why the dispatch's restore blocks live outside it.
#[test]
fn a_mixed_function_inside_try_finally_runs() {
    const MAIN: &str = r#"
        import kotlinx.coroutines.runBlocking
        suspend fun one(v: Int): Int { kotlinx.coroutines.yield(); return v + 1 }
        suspend fun guardedMixed(v: Int): Int {
            var acc = 0
            try {
                acc = acc + one(v)
                acc = acc + twice(v) { one(it) }
            } finally {
                acc = acc + 1
            }
            return acc
        }
        fun box(): String = runBlocking {
            val n = guardedMixed(10)
            if (n == 35) "OK" else "FAIL: " + n
        }
    "#;
    let Some(output) = run("probe_mixed_try", MAIN) else {
        return;
    };
    assert_eq!(output, "OK");
}

/// A PRIVATE top-level function's machine. Its continuation cannot name the method either, so it
/// re-enters through the same synthetic static a private member uses — with no receiver.
#[test]
fn a_suspension_inside_a_spliced_lambda_of_a_private_function_runs() {
    const MAIN: &str = r#"
        import kotlinx.coroutines.runBlocking
        suspend fun one(v: Int): Int { kotlinx.coroutines.yield(); return v + 1 }
        private suspend fun step(v: Int): Int = twice(v) { one(it) }
        fun box(): String = runBlocking {
            val n = step(10)
            if (n == 23) "OK" else "FAIL: " + n
        }
    "#;
    let Some(output) = run("suspend_spliced_private_fun", MAIN) else {
        return;
    };
    assert_eq!(output, "OK");
}

/// `sumOf` holds its accumulator on the operand stack across the call it makes. The stack does not
/// survive a suspension, so the machine saves it into locals and puts it back on both paths out.
#[test]
fn a_suspension_under_a_dependency_operand_stack_runs() {
    const MAIN: &str = r#"
        import kotlinx.coroutines.runBlocking
        suspend fun one(v: Int): Int { kotlinx.coroutines.yield(); return v + 1 }
        suspend fun total(xs: List<Int>): Int = xs.sumOf { one(it) }
        fun box(): String = runBlocking {
            val n = total(listOf(1, 2, 3))
            if (n == 9) "OK" else "FAIL: " + n
        }
    "#;
    let Some(output) = run("suspend_spliced_understack", MAIN) else {
        return;
    };
    assert_eq!(output, "OK");
}

/// `any` leaves the loop early, so the frames on the far side of the suspension claim locals the
/// spliced body assigned and no frame before it describes.
#[test]
fn a_suspension_in_a_short_circuiting_stdlib_loop_runs() {
    const MAIN: &str = r#"
        import kotlinx.coroutines.runBlocking
        suspend fun one(v: Int): Int { kotlinx.coroutines.yield(); return v + 1 }
        suspend fun anyBig(xs: List<Int>): Boolean = xs.any { one(it) > 2 }
        fun box(): String = runBlocking {
            val hit = anyBig(listOf(1, 2))
            if (hit) "OK" else "FAIL"
        }
    "#;
    let Some(output) = run("suspend_spliced_any", MAIN) else {
        return;
    };
    assert_eq!(output, "OK");
}

/// `takeIf` returns the receiver or null, and `mapNotNull` builds a list: both keep a reference the
/// resume has to hand back.
#[test]
fn suspensions_in_stdlib_bodies_that_carry_a_reference_run() {
    const MAIN: &str = r#"
        import kotlinx.coroutines.runBlocking
        suspend fun one(v: Int): Int { kotlinx.coroutines.yield(); return v + 1 }
        suspend fun kept(v: Int): Int? = v.takeIf { one(it) > 5 }
        suspend fun mapped(xs: List<Int>): List<Int> = xs.mapNotNull { one(it) }
        fun box(): String = runBlocking {
            val k = kept(9)
            val m = mapped(listOf(1, 2))
            if (k == 9 && m == listOf(2, 3)) "OK" else "FAIL: " + k + " " + m
        }
    "#;
    let Some(output) = run("suspend_spliced_reference_bodies", MAIN) else {
        return;
    };
    assert_eq!(output, "OK");
}

/// The dependency's stack prefix is found by matching a marker's position against the site the
/// splice laid out — two coordinate systems, and they only agree when nothing precedes the call.
/// Statements before it used to lose the prefix entirely.
#[test]
fn a_suspension_under_a_prefix_runs_with_statements_before_it() {
    const MAIN: &str = r#"
        import kotlinx.coroutines.runBlocking
        suspend fun one(v: Int): Int { kotlinx.coroutines.yield(); return v + 1 }
        suspend fun padded(): Int {
            val a = 1
            val b = 2
            val c = 3
            return twice(1) { one(it) } + a + b + c
        }
        fun box(): String = runBlocking {
            val n = padded()
            if (n == 11) "OK" else "FAIL: " + n
        }
    "#;
    let Some(output) = run("suspend_spliced_padded", MAIN) else {
        return;
    };
    assert_eq!(output, "OK");
}

/// A spliced body that splices another: the outer call's values sit UNDER the inner call's, and the
/// resume has to restore both.
#[test]
fn a_suspension_under_two_nested_prefixes_runs() {
    const MAIN: &str = r#"
        import kotlinx.coroutines.runBlocking
        suspend fun one(v: Int): Int { kotlinx.coroutines.yield(); return v + 1 }
        suspend fun nested(): Int = twice(1) { x -> twice(x) { y -> one(y) } }
        fun box(): String = runBlocking {
            val n = nested()
            if (n == 12) "OK" else "FAIL: " + n
        }
    "#;
    let Some(output) = run("suspend_spliced_nested_prefix", MAIN) else {
        return;
    };
    assert_eq!(output, "OK");
}

/// A REFERENCE on the dependency's stack — the splice names its class by constant-pool index, which
/// a spill field's descriptor cannot use.
#[test]
fn a_reference_on_the_dependency_stack_survives_a_suspension() {
    const MAIN: &str = r#"
        import kotlinx.coroutines.runBlocking
        suspend fun one(v: Int): Int { kotlinx.coroutines.yield(); return v + 1 }
        suspend fun held(): Int = holdRef { one(1) }
        fun box(): String = runBlocking {
            val n = held()
            if (n == 2) "OK" else "FAIL: " + n
        }
    "#;
    let Some(output) = run("suspend_spliced_ref_prefix", MAIN) else {
        return;
    };
    assert_eq!(output, "OK");
}

/// Two overloads each own a machine; they must not share one continuation class, whose spill layout
/// belongs to exactly one of them.
#[test]
fn overloaded_suspend_functions_get_their_own_continuations() {
    const MAIN: &str = r#"
        import kotlinx.coroutines.runBlocking
        suspend fun one(v: Int): Int { kotlinx.coroutines.yield(); return v + 1 }
        suspend fun f(a: Int): Int = twice(a) { one(it) }
        suspend fun f(): Int = f(1) + f(2)
        fun box(): String = runBlocking {
            val n = f()
            if (n == 12) "OK" else "FAIL: " + n
        }
    "#;
    let Some(output) = run("suspend_spliced_overloads", MAIN) else {
        return;
    };
    assert_eq!(output, "OK");
}

/// The `catch` arm actually runs: the second iteration throws before its suspension. A spliced
/// body's `try` ranges have to reach the enclosing method's exception table for this to hold —
/// the splice used to drop them, leaving the handler as dead code.
#[test]
fn a_value_try_catches_a_throw_on_the_direct_path() {
    const MAIN: &str = r#"
        import kotlinx.coroutines.runBlocking
        suspend fun one(v: Int): Int { kotlinx.coroutines.yield(); return v + 1 }
        suspend fun guarded(): Int = twice(1) { x ->
            try { if (x == 2) throw IllegalStateException("x"); one(x) } catch (e: Exception) { -1 }
        }
        fun box(): String = runBlocking {
            val n = guarded()
            if (n == 1) "OK" else "FAIL: " + n
        }
    "#;
    let Some(output) = run("suspend_spliced_value_try_throws", MAIN) else {
        return;
    };
    assert_eq!(output, "OK");
}

/// The callee fails AFTER suspending: the failure arrives as the resumption's `Result`, and is
/// rethrown at the resume point INSIDE the body — inside the `try` — so the `catch` sees it. A
/// rethrow in the dispatch's restore block, outside every range the body declares, would escape.
#[test]
fn a_value_try_catches_a_failed_resumption() {
    const MAIN: &str = r#"
        import kotlinx.coroutines.runBlocking
        suspend fun boom(v: Int): Int { kotlinx.coroutines.yield(); throw IllegalStateException("b" + v) }
        suspend fun guarded(): Int = twice(1) { x -> try { boom(x) } catch (e: IllegalStateException) { -1 } }
        fun box(): String = runBlocking {
            val n = guarded()
            if (n == -2) "OK" else "FAIL: " + n
        }
    "#;
    let Some(output) = run("suspend_spliced_value_try_resume_throws", MAIN) else {
        return;
    };
    assert_eq!(output, "OK");
}
