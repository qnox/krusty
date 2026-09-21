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

inline fun <R> accumulate(xs: List<Int>, initial: R, f: (R, Int) -> R): R {
    var acc = initial
    for (x in xs) acc = f(acc, x)
    return acc
}

inline fun both3(f: (Int) -> Int): Int = f(1) + f(2)

inline fun twoSites(f: (Int) -> Int): Int {
    val a = f(1)
    val s = "x"
    val b = f(2)
    return a + b + s.length
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

/// The suspension is an operand of a write to a LOCAL DECLARED IN THE SPLICED BODY: `s = s + one(x)`
/// reaches the suspension with `s` on the stack, which the `areturn` a suspension leaves through
/// destroys. The hoist has to snapshot `s` into a temp first, and typing that snapshot means seeing
/// the lambda's own locals — the body is numbered in the lambda's own value space, not the
/// enclosing function's.
#[test]
fn a_suspension_read_modify_writing_a_spliced_body_local_runs() {
    const MAIN: &str = r#"
        import kotlinx.coroutines.runBlocking
        suspend fun one(v: Int): Int { kotlinx.coroutines.yield(); return v + 1 }
        suspend fun a(): Int = twice(1) { x -> var s = 0; s = s + one(x); s }
        fun box(): String = runBlocking {
            val n = a()
            if (n == 5) "OK" else "FAIL: " + n
        }
    "#;
    let Some(output) = run("suspend_spliced_rmw_local", MAIN) else {
        return;
    };
    assert_eq!(output, "OK");
}

/// The compound form of the same shape: `s += one(x)` desugars to the read-modify-write above.
#[test]
fn a_suspension_compound_assigned_to_a_spliced_body_local_runs() {
    const MAIN: &str = r#"
        import kotlinx.coroutines.runBlocking
        suspend fun one(v: Int): Int { kotlinx.coroutines.yield(); return v + 1 }
        suspend fun a(): Int = twice(1) { x -> var s = 0; s += one(x); s }
        fun box(): String = runBlocking {
            val n = a()
            if (n == 5) "OK" else "FAIL: " + n
        }
    "#;
    let Some(output) = run("suspend_spliced_compound_local", MAIN) else {
        return;
    };
    assert_eq!(output, "OK");
}

/// An array store: the array reference and the index sit on the stack under the suspension, and the
/// array is a spliced-body local too.
#[test]
fn a_suspension_stored_into_a_spliced_body_array_runs() {
    const MAIN: &str = r#"
        import kotlinx.coroutines.runBlocking
        suspend fun one(v: Int): Int { kotlinx.coroutines.yield(); return v + 1 }
        suspend fun b(): Int = twice(1) { x -> val arr = IntArray(1); arr[0] = one(x); arr[0] }
        fun box(): String = runBlocking {
            val n = b()
            if (n == 5) "OK" else "FAIL: " + n
        }
    "#;
    let Some(output) = run("suspend_spliced_array_local", MAIN) else {
        return;
    };
    assert_eq!(output, "OK");
}

/// A body local declared AFTER a capture: the lambda's numbering is `tag` at 0, `x` at 1, `s` at 2,
/// so `s` sits at an index the enclosing function (`k` at 0, `tag` at 1) never declared.
#[test]
fn a_body_local_after_a_capture_is_typed_in_the_lambda_numbering() {
    const MAIN: &str = r#"
        import kotlinx.coroutines.runBlocking
        suspend fun one(v: Int): Int { kotlinx.coroutines.yield(); return v + 1 }
        suspend fun c(k: Int, tag: String): Int = twice(k) { x -> var s = tag.length; s = s + one(x); s }
        fun box(): String = runBlocking {
            val n = c(1, "ab")
            if (n == 9) "OK" else "FAIL: " + n
        }
    "#;
    let Some(output) = run("suspend_spliced_capture_numbering", MAIN) else {
        return;
    };
    assert_eq!(output, "OK");
}

/// The capture ITSELF is the snapshot: `arr[0] = one(x)` puts the array under the suspension, and
/// `arr` is capture 0 — an `IntArray` — where the enclosing function's parameter 0 is `k: Int`.
/// Typed from the enclosing table, that snapshot would be an `Int` temp holding an array reference:
/// a miscompile, not a decline. Typed in the lambda's numbering it is the array it is.
#[test]
fn a_captured_array_under_a_suspension_is_typed_as_the_capture() {
    const MAIN: &str = r#"
        import kotlinx.coroutines.runBlocking
        suspend fun one(v: Int): Int { kotlinx.coroutines.yield(); return v + 1 }
        suspend fun d(k: Int, arr: IntArray): Int = twice(k) { x -> arr[0] = one(x); arr[0] }
        fun box(): String = runBlocking {
            val n = d(1, IntArray(1))
            if (n == 5) "OK" else "FAIL: " + n
        }
    "#;
    let Some(output) = run("suspend_spliced_captured_array", MAIN) else {
        return;
    };
    assert_eq!(output, "OK");
}

/// An inline function that invokes its lambda at TWO sites splices the body twice. Each copy is its
/// own state: two positions cannot share one ordinal, one spill set and one `label`.
#[test]
fn a_lambda_invoked_at_two_sites_suspends_at_each() {
    const MAIN: &str = r#"
        import kotlinx.coroutines.runBlocking
        suspend fun one(v: Int): Int { kotlinx.coroutines.yield(); return v + 1 }
        suspend fun g(): Int = both3 { one(it) }
        fun box(): String = runBlocking {
            val n = g()
            if (n == 5) "OK" else "FAIL: " + n
        }
    "#;
    let Some(output) = run("suspend_spliced_two_sites", MAIN) else {
        return;
    };
    assert_eq!(output, "OK");
}

/// The two sites hold different locals across their suspension — the second one has the first
/// site's result and a reference in scope — so each state needs its own spill set.
#[test]
fn two_sites_with_different_live_sets_each_spill_their_own() {
    const MAIN: &str = r#"
        import kotlinx.coroutines.runBlocking
        suspend fun one(v: Int): Int { kotlinx.coroutines.yield(); return v + 1 }
        suspend fun g(): Int = twoSites { one(it) }
        fun box(): String = runBlocking {
            val n = g()
            if (n == 6) "OK" else "FAIL: " + n
        }
    "#;
    let Some(output) = run("suspend_spliced_two_sites_live", MAIN) else {
        return;
    };
    assert_eq!(output, "OK");
}

/// A stdlib function with two selector sites: `maxOf` calls the selector once before its loop and
/// once inside it.
#[test]
fn a_suspension_inside_a_spliced_stdlib_max_of_selector_runs() {
    const MAIN: &str = r#"
        import kotlinx.coroutines.runBlocking
        suspend fun one(v: Int): Int { kotlinx.coroutines.yield(); return v + 1 }
        suspend fun f(xs: List<Int>): Int = xs.maxOf { one(it) }
        fun box(): String = runBlocking {
            val n = f(listOf(1, 5, 3))
            if (n == 6) "OK" else "FAIL: " + n
        }
    "#;
    let Some(output) = run("suspend_spliced_max_of", MAIN) else {
        return;
    };
    assert_eq!(output, "OK");
}

/// A generic accumulator the dependency's loop REWRITES across the suspension: its type at the
/// join is the loop's merge — `Object`, where the entry edge holds the boxed initial value and the
/// back edge the lambda's boxed result — which no single frame in the body states. The spill's type
/// is computed by the forward verification-type analysis, not read off the nearest frame.
#[test]
fn an_accumulator_rewritten_across_a_spliced_suspension_runs() {
    const MAIN: &str = r#"
        import kotlinx.coroutines.runBlocking
        suspend fun one(v: Int): Int { kotlinx.coroutines.yield(); return v + 1 }
        suspend fun total(xs: List<Int>): Int = accumulate(xs, 0) { acc, b -> acc + one(b) }
        suspend fun joined(xs: List<Int>): String = accumulate(xs, "") { acc, b -> acc + one(b) }
        fun box(): String = runBlocking {
            val xs = listOf(1, 2, 3)
            val n = total(xs)
            val s = joined(xs)
            if (n == 9 && s == "234") "OK" else "FAIL: " + n + " " + s
        }
    "#;
    let Some(output) = run("suspend_spliced_accumulator", MAIN) else {
        return;
    };
    assert_eq!(output, "OK");
}

/// The same shape through the stdlib's `fold` and `reduce`, whose bodies are whatever distribution
/// is provisioned: the values are checked, not only that the class verifies, because a machine
/// that restores the accumulator under the wrong type fails at the `iadd` that consumes it.
#[test]
fn stdlib_fold_and_reduce_with_a_suspending_operation_run() {
    const MAIN: &str = r#"
        import kotlinx.coroutines.runBlocking
        suspend fun one(v: Int): Int { kotlinx.coroutines.yield(); return v + 1 }
        suspend fun a(xs: List<Int>): Int = xs.fold(0) { acc, b -> acc + one(b) }
        suspend fun b(xs: List<Int>): Int = xs.reduce { acc, b -> acc + one(b) }
        suspend fun c(xs: List<Int>): String = xs.fold("") { acc, b -> acc + one(b) }
        suspend fun d(xs: List<Int>): Long = xs.fold(0L) { acc, b -> acc + one(b) }
        suspend fun e(xs: List<Int>): List<Int> = xs.fold(listOf()) { acc, b -> acc + one(b) }
        suspend fun f(xs: List<Int>): Int = xs.foldIndexed(0) { i, acc, b -> acc + i + one(b) }
        suspend fun g(xs: List<Int>): Int = xs.reduce { acc, b -> maxOf(acc, one(b)) }
        suspend fun h(xs: List<Int>): Int = xs.sumOf { one(it) }
        suspend fun i(xs: List<Int>): String = xs.fold("") { acc, b -> val t = acc + one(b); t }
        suspend fun j(xs: List<Int>): Int = xs.foldRight(0) { b, acc -> acc + one(b) }
        fun box(): String = runBlocking {
            val xs = listOf(1, 2, 3)
            val got = listOf(a(xs), b(xs), c(xs), d(xs), e(xs), f(xs), g(xs), h(xs), i(xs), j(xs))
            val want = listOf<Any>(9, 8, "234", 9L, listOf(2, 3, 4), 12, 4, 9, "234", 9)
            if (got == want) "OK" else "FAIL: " + got
        }
    "#;
    let Some(output) = run("suspend_spliced_fold_reduce", MAIN) else {
        return;
    };
    assert_eq!(output, "OK");
}

/// A snapshot of a spliced lambda's OWN parameter, taken before a later operand suspends, is typed
/// by the lambda's numbering, not the enclosing function's: `acc` here is value 0 of the lambda,
/// and value 0 of `total` is the list. Typed as the list, the temp was boxed on the way in and
/// consumed as an `int` on the way out.
#[test]
fn a_snapshot_of_a_spliced_lambda_parameter_is_typed_by_the_lambda() {
    const MAIN: &str = r#"
        import kotlinx.coroutines.runBlocking
        suspend fun one(v: Int): Int { kotlinx.coroutines.yield(); return v + 1 }
        suspend fun total(xs: List<Int>): Int = accumulate(xs, 0) { acc, b -> acc * 10 + one(b) }
        fun box(): String = runBlocking {
            val n = total(listOf(1, 2, 3))
            if (n == 234) "OK" else "FAIL: " + n
        }
    "#;
    let Some(output) = run("suspend_spliced_lambda_param_snapshot", MAIN) else {
        return;
    };
    assert_eq!(output, "OK");
}

/// A local the verifier holds as `null` at the suspension — `var s: String? = null` with no
/// frame between its store and a branchless splice — is a constant, not a field: restored as
/// `Object` it would fail the frame after the join, which claims the declared `String`.
#[test]
fn a_null_local_across_a_spliced_suspension_is_rematerialized() {
    const MAIN: &str = r#"
        import kotlinx.coroutines.runBlocking
        suspend fun one(v: Int): Int { kotlinx.coroutines.yield(); return v + 1 }
        suspend fun keep(c: Boolean): String {
            var s: String? = null
            val n = run { one(1) }
            if (c) s = "x" + n
            return s ?: "none"
        }
        fun box(): String = runBlocking {
            val a = keep(true)
            val b = keep(false)
            if (a == "x2" && b == "none") "OK" else "FAIL: " + a + " " + b
        }
    "#;
    let Some(output) = run("suspend_spliced_null_local", MAIN) else {
        return;
    };
    assert_eq!(output, "OK");
}

/// A `null` literal left on the operand stack under the suspension — a constant operand is never
/// snapshot — is carried the same way, and pushed back as `null` so the call it was an argument
/// of still verifies against its `String` parameter.
#[test]
fn a_null_operand_under_a_spliced_suspension_is_rematerialized() {
    const MAIN: &str = r#"
        import kotlinx.coroutines.runBlocking
        suspend fun one(v: Int): Int { kotlinx.coroutines.yield(); return v + 1 }
        fun label(a: String?, b: Int): String = (a ?: "n") + b
        suspend fun call(): String = run { label(null, one(1)) }
        fun box(): String = runBlocking {
            val s = call()
            if (s == "n2") "OK" else "FAIL: " + s
        }
    "#;
    let Some(output) = run("suspend_spliced_null_operand", MAIN) else {
        return;
    };
    assert_eq!(output, "OK");
}

/// A `try` that produces the spliced body's VALUE, with the suspension as the `try` arm's value.
/// The arm's raw `Object` result must be adapted before the `try`'s result slot takes it — the
/// value-`try` desugar that does this for a function body stops at every lambda, so a spliced body
/// gets its own.
#[test]
fn a_value_try_whose_arm_is_the_suspension_runs() {
    const MAIN: &str = r#"
        import kotlinx.coroutines.runBlocking
        suspend fun one(v: Int): Int { kotlinx.coroutines.yield(); return v + 1 }
        suspend fun guarded(): Int = twice(1) { x -> try { one(x) } catch (e: Exception) { -1 } }
        fun box(): String = runBlocking {
            val n = guarded()
            if (n == 5) "OK" else "FAIL: " + n
        }
    "#;
    let Some(output) = run("suspend_spliced_value_try", MAIN) else {
        return;
    };
    assert_eq!(output, "OK");
}

/// The same, with the suspension nested in the arm's value expression: the desugar binds the arm
/// through a typed local, and the hoist that follows lifts the suspension out of the expression.
#[test]
fn a_value_try_whose_arm_computes_with_the_suspension_runs() {
    const MAIN: &str = r#"
        import kotlinx.coroutines.runBlocking
        suspend fun one(v: Int): Int { kotlinx.coroutines.yield(); return v + 1 }
        suspend fun guarded(): Int = twice(1) { x -> try { one(x) + 0 } catch (e: Exception) { -1 } }
        fun box(): String = runBlocking {
            val n = guarded()
            if (n == 5) "OK" else "FAIL: " + n
        }
    "#;
    let Some(output) = run("suspend_spliced_value_try_expr", MAIN) else {
        return;
    };
    assert_eq!(output, "OK");
}

/// The shape that worked before the desugar reached spliced bodies — the arm binds the suspension
/// to a local first — and must keep working through it.
#[test]
fn a_value_try_whose_arm_binds_the_suspension_first_runs() {
    const MAIN: &str = r#"
        import kotlinx.coroutines.runBlocking
        suspend fun one(v: Int): Int { kotlinx.coroutines.yield(); return v + 1 }
        suspend fun guarded(): Int = twice(1) { x -> try { val v = one(x); v } catch (e: Exception) { -1 } }
        fun box(): String = runBlocking {
            val n = guarded()
            if (n == 5) "OK" else "FAIL: " + n
        }
    "#;
    let Some(output) = run("suspend_spliced_value_try_bound", MAIN) else {
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

/// A `try`/`finally` around the suspension, with the callee failing after it suspended: the
/// catch-all handler covers the resume point, so the finalizer runs on that path too.
#[test]
fn a_value_try_finally_runs_its_finalizer_on_a_failed_resumption() {
    const MAIN: &str = r#"
        import kotlinx.coroutines.runBlocking
        var trace = ""
        suspend fun boom(v: Int): Int { kotlinx.coroutines.yield(); throw IllegalStateException("b" + v) }
        suspend fun guarded(): Int = twice(1) { x ->
            try { boom(x) } catch (e: IllegalStateException) { -1 } finally { trace = trace + x }
        }
        fun box(): String = runBlocking {
            val n = guarded()
            if (n == -2 && trace == "12") "OK" else "FAIL: " + n + " " + trace
        }
    "#;
    let Some(output) = run("suspend_spliced_value_try_finally", MAIN) else {
        return;
    };
    assert_eq!(output, "OK");
}

/// The suspension sits in the `catch` arm: reached only through the handler, whose frame the
/// state's restore has to agree with.
#[test]
fn a_value_try_whose_catch_arm_suspends_runs() {
    const MAIN: &str = r#"
        import kotlinx.coroutines.runBlocking
        suspend fun one(v: Int): Int { kotlinx.coroutines.yield(); return v + 1 }
        suspend fun guarded(): Int = twice(1) { x ->
            try { if (x == 2) throw IllegalStateException("x"); x } catch (e: Exception) { one(-x) }
        }
        fun box(): String = runBlocking {
            val n = guarded()
            if (n == 0) "OK" else "FAIL: " + n
        }
    "#;
    let Some(output) = run("suspend_spliced_value_try_catch_suspends", MAIN) else {
        return;
    };
    assert_eq!(output, "OK");
}

/// A non-local `return` of a value-`try` from the spliced body. The desugar reaches this shape,
/// but the machine cannot emit a non-local return yet (it has to yield the CPS `Object`), so the
/// function keeps the diagnostic it had before the machine existed rather than a class that fails
/// verification.
#[test]
fn a_non_local_return_of_a_value_try_still_declines() {
    const MAIN: &str = r#"
        import kotlinx.coroutines.runBlocking
        suspend fun one(v: Int): Int { kotlinx.coroutines.yield(); return v + 1 }
        suspend fun early(): Int {
            twice(1) { x -> return try { one(x) } catch (e: Exception) { -1 } }
            return 0
        }
        fun box(): String = runBlocking { "" + early() }
    "#;
    let jdk = common::jdk_modules();
    let Some(libout) = common::compile_lib_ref("suspend_spliced_nonlocal_return_try", LIB) else {
        return;
    };
    let cp = [
        libout,
        common::stdlib_jar(),
        common::coroutines_jar(),
        jdk.clone(),
    ];
    let outcome = common::backend_outcome_in_process(MAIN, "Main", &cp, Some(jdk.as_path()))
        .expect("the source is frontend-valid");
    match outcome {
        common::BackendOutcome::Rejected(diagnostics) => assert!(
            diagnostics
                .iter()
                .any(|d| d.contains("call arity mismatch")),
            "expected the pre-machine diagnostic, got {diagnostics:?}"
        ),
        common::BackendOutcome::Emitted => panic!("a non-local return under the machine emitted"),
    }
}
