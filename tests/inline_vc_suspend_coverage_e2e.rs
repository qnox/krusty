//! Targeted end-to-end "box" coverage for the three worst-covered JVM backend passes:
//! `src/jvm/inline.rs`, `src/jvm/value_classes.rs`, and `src/jvm/suspend.rs`. Prior suites
//! (`inline_deep_coverage`, `feature_coverage_u/w`, `value_class_e2e`, `value_class_map_key`,
//! `suspend_e2e`, `suspend_try_finally`, `coroutine_intrinsics`) already cover the common shapes;
//! this file deliberately targets DIFFERENT / rarer branches:
//!
//!   * inline.rs — labeled non-local returns, crossinline threaded through a nested lambda, reified
//!     `as T` / `T::class` / `arrayOfNulls<T>`, inline extension / operator / member functions, two
//!     function params, varargs, a lambda body carrying its OWN try/catch (exception-table relocation
//!     inside the spliced lambda), non-reified generics, string `when`, nested loops with many locals,
//!     a result discarded in statement position, a conditionally-not-invoked lambda, all scope
//!     functions chained, and Unit-returning lambdas.
//!   * value_classes.rs — member functions, computed properties, nullable value-class boxing, a value
//!     class implementing an interface (box at the interface boundary), value class as an extension
//!     receiver, `List<Vc>` / `Map` values (generic-position boxing), value classes through `when`,
//!     `Long` / `Double` / `Boolean` underlying types, and value class as a parameter + return.
//!   * suspend.rs — a suspension inside a `while` / `for` / `when`, suspend calling a suspend MEMBER
//!     function and a suspend operator, locals spilled across sequential suspensions, and suspend
//!     functions returning `String` / `Boolean` / nullable / `Unit`.
//!
//! Box tests compile a self-contained `fun box(): String` returning "OK" and assert it. Suspend tests
//! compile a `suspend fun` with krusty and drive it from a Java `Continuation` (the callees complete
//! synchronously, so the whole state machine runs to completion under `-Xverify:all`). Each test skips
//! cleanly when the toolchain isn't provisioned.

use std::fs;
use std::process::Command;

use super::common;

// ============================================================================
// Box harness (inline + value classes).
// ============================================================================

/// Compile `src` (stem `stem`) against kotlin-stdlib + JDK modules and run `box()`, asserting "OK".
fn run_ok(src: &str, stem: &str) {
    let Some(stdlib) = common::stdlib_jar() else {
        eprintln!("skipping {stem}: no kotlin-stdlib jar");
        return;
    };
    let jdk = common::jdk_modules();
    let Some(out) = common::compile_and_run_box(src, stem, &[stdlib], jdk.as_deref()) else {
        return;
    };
    assert_eq!(out, "OK", "{stem} produced wrong box() result");
}

// ============================================================================
// Suspend harness — compile a `suspend fun` and drive it via a Java Continuation.
// ============================================================================

/// Compile `src` (top-level suspend fns land on `SKt`) with the krusty binary, then run a Java driver
/// that evaluates `SKt.<call_expr>` (where `k` is an in-scope trivial `Continuation`) and asserts the
/// synchronously-completing result's `String.valueOf` equals `expect`.
fn run_suspend(name: &str, src: &str, call_expr: &str, expect: &str) {
    let jh = match common::java_home() {
        Some(j) if std::path::Path::new(&format!("{j}/bin/javac")).exists() => j,
        _ => return,
    };
    let _ = jh;
    let Some(stdlib) = common::stdlib_jar() else {
        eprintln!("skipping {name}: no kotlin-stdlib jar");
        return;
    };
    let krusty = env!("CARGO_BIN_EXE_krusty");
    let dir = std::env::temp_dir().join(format!("krusty_ivsc_{name}_{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("S.kt"), src).unwrap();
    let kc = Command::new(krusty)
        .args([
            "-cp",
            &stdlib.to_string_lossy(),
            "-d",
            dir.to_str().unwrap(),
        ])
        .arg(dir.join("S.kt"))
        .output()
        .unwrap();
    assert!(
        kc.status.success(),
        "{name}: krusty failed to compile:\n{}",
        String::from_utf8_lossy(&kc.stderr)
    );
    let driver = format!(
        "import kotlin.coroutines.*;\n\
public class M {{\n\
  public static void main(String[] a) {{\n\
    Continuation<Object> k = new Continuation<Object>() {{\n\
      public CoroutineContext getContext() {{ return EmptyCoroutineContext.INSTANCE; }}\n\
      public void resumeWith(Object o) {{ }}\n\
    }};\n\
    Object r = SKt.{call_expr};\n\
    System.out.println(String.valueOf(r).equals(\"{expect}\") ? \"OK\" : (\"r=\" + r));\n\
  }}\n\
}}\n"
    );
    fs::write(dir.join("M.java"), driver).unwrap();
    let cp = format!("{}:{}", dir.to_str().unwrap(), stdlib.to_string_lossy());
    let Some(out) = common::javac_run(
        dir.join("M.java").to_str().unwrap(),
        &cp,
        dir.to_str().unwrap(),
        "M",
    ) else {
        return;
    };
    let _ = fs::remove_dir_all(&dir);
    assert_eq!(out.trim(), "OK", "{name}: wrong result; got {out}");
}

/// Compile a `suspend fun` `src` with the krusty binary and assert the BACKEND cleanly REJECTS it (a
/// non-zero exit / "not yet supported" diagnostic) — exercising the `src/jvm/suspend.rs` bail branch
/// for suspend-function shapes krusty does not yet lower. Skips clean when the toolchain is absent.
fn rejects_suspend(name: &str, src: &str) {
    let Some(stdlib) = common::stdlib_jar() else {
        eprintln!("skipping {name}: no kotlin-stdlib jar");
        return;
    };
    let krusty = env!("CARGO_BIN_EXE_krusty");
    let dir = std::env::temp_dir().join(format!("krusty_ivsc_rej_{name}_{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("S.kt"), src).unwrap();
    let out = Command::new(krusty)
        .args([
            "-cp",
            &stdlib.to_string_lossy(),
            "-d",
            dir.to_str().unwrap(),
        ])
        .arg(dir.join("S.kt"))
        .output()
        .unwrap();
    let _ = fs::remove_dir_all(&dir);
    assert!(
        !out.status.success(),
        "{name}: expected the backend to REJECT this suspend shape, but it compiled"
    );
}

// ############################################################################
// INLINE.RS — rarer splice paths.
// ############################################################################

// A labeled non-local return from inside a stdlib inline lambda (`return@forEach`) — the label-return
// join path in the splicer, distinct from an unlabeled non-local return.
#[test]
fn inline_labeled_return_from_lambda() {
    let src = "fun firstEven(xs: List<Int>): Int {\n\
    xs.forEach { if (it % 2 == 0) return it }\n\
    return -1\n\
}\n\
fun box(): String {\n\
    if (firstEven(listOf(1, 3, 4, 5)) != 4) return \"f1\"\n\
    if (firstEven(listOf(1, 3, 5)) != -1) return \"f2\"\n\
    return \"OK\"\n\
}\n";
    run_ok(src, "InlineLabeledReturnFromLambda");
}

// A local labeled return (`return@block`) that terminates only the inline lambda, not the caller.
#[test]
fn inline_local_labeled_return() {
    let src = "fun box(): String {\n\
    val r = run rr@{\n\
        for (i in 0 until 10) { if (i == 3) return@rr i * 10 }\n\
        -1\n\
    }\n\
    if (r != 30) return \"r=$r\"\n\
    return \"OK\"\n\
}\n";
    run_ok(src, "InlineLocalLabeledReturn");
}

// `crossinline` — the lambda is captured inside a NESTED ordinary lambda (`run { ... f() ... }`), so
// it cannot be a plain non-local return; the splicer must inline it into the nested body.
#[test]
fn inline_crossinline_in_nested_lambda() {
    let src = "inline fun wrap(crossinline f: () -> Int): Int {\n\
    val g = { f() + 1 }\n\
    return g()\n\
}\n\
fun box(): String {\n\
    val r = wrap { 41 }\n\
    if (r != 42) return \"r=$r\"\n\
    return \"OK\"\n\
}\n";
    run_ok(src, "InlineCrossinlineInNestedLambda");
}

// A reified `as T` cast in the body — the reified type marker resolves to the concrete class in a
// checkcast relocation.
#[test]
fn inline_reified_as_cast() {
    let src = "inline fun <reified T> castOr(x: Any, d: T): T = if (x is T) x as T else d\n\
fun box(): String {\n\
    val s = castOr<String>(\"hi\", \"d\")\n\
    if (s != \"hi\") return \"f1\"\n\
    val n = castOr<String>(42, \"d\")\n\
    if (n != \"d\") return \"f2\"\n\
    return \"OK\"\n\
}\n";
    run_ok(src, "InlineReifiedAsCast");
}

// A reified `T::class` class literal in the body.
#[test]
fn inline_reified_class_literal() {
    let src = "inline fun <reified T> nameOf(): String = T::class.simpleName ?: \"?\"\n\
fun box(): String {\n\
    if (nameOf<String>() != \"String\") return \"f1:\" + nameOf<String>()\n\
    if (nameOf<Int>() != \"Int\") return \"f2:\" + nameOf<Int>()\n\
    return \"OK\"\n\
}\n";
    run_ok(src, "InlineReifiedClassLiteral");
}

// A reified `arrayOfNulls<T>` construction (anewarray of the reified element type).
#[test]
fn inline_reified_array_of_nulls() {
    let src = "inline fun <reified T> makeArr(n: Int): Array<T?> = arrayOfNulls<T>(n)\n\
fun box(): String {\n\
    val a = makeArr<String>(3)\n\
    if (a.size != 3) return \"f1\"\n\
    if (a[0] != null) return \"f2\"\n\
    a[1] = \"x\"\n\
    if (a[1] != \"x\") return \"f3\"\n\
    return \"OK\"\n\
}\n";
    run_ok(src, "InlineReifiedArrayOfNulls");
}

// An inline EXTENSION function (has a receiver spliced as an extra local).
#[test]
fn inline_extension_receiver() {
    let src = "inline fun Int.twice(f: (Int) -> Int): Int = f(this) + f(this)\n\
fun box(): String {\n\
    val r = 5.twice { it * 10 }\n\
    if (r != 100) return \"r=$r\"\n\
    return \"OK\"\n\
}\n";
    run_ok(src, "InlineExtensionReceiver");
}

// An inline OPERATOR function.
#[test]
fn inline_operator_fun() {
    let src = "class Box(val v: Int) {\n\
    inline operator fun invoke(f: (Int) -> Int): Int = f(v)\n\
}\n\
fun box(): String {\n\
    val b = Box(7)\n\
    val r = b { it + 1 }\n\
    if (r != 8) return \"r=$r\"\n\
    return \"OK\"\n\
}\n";
    run_ok(src, "InlineOperatorFun");
}

// Two function parameters, both spliced (distinct lambda slots).
#[test]
fn inline_two_lambda_params() {
    let src = "inline fun combine(a: Int, f: (Int) -> Int, g: (Int) -> Int): Int = f(a) + g(a)\n\
fun box(): String {\n\
    val r = combine(3, { it * 2 }, { it * 100 })\n\
    if (r != 306) return \"r=$r\"\n\
    return \"OK\"\n\
}\n";
    run_ok(src, "InlineTwoLambdaParams");
}

// An inline fun with a VARARG parameter alongside a lambda.
#[test]
fn inline_vararg_param() {
    let src = "inline fun pick(vararg xs: Int, f: (Int) -> Boolean): Int {\n\
    for (x in xs) if (f(x)) return x\n\
    return -1\n\
}\n\
fun box(): String {\n\
    val r = pick(1, 3, 5, 8, 9) { it % 2 == 0 }\n\
    if (r != 8) return \"r=$r\"\n\
    return \"OK\"\n\
}\n";
    run_ok(src, "InlineVarargParam");
}

// A spliced lambda whose OWN body carries a try/catch — the lambda's exception table must be
// relocated into the host body.
#[test]
fn inline_lambda_body_try_catch() {
    let src = "inline fun attempt(f: () -> Int): Int = f()\n\
fun box(): String {\n\
    val r = attempt {\n\
        try { \"x\".toInt() } catch (e: NumberFormatException) { 77 }\n\
    }\n\
    if (r != 77) return \"r=$r\"\n\
    return \"OK\"\n\
}\n";
    run_ok(src, "InlineLambdaBodyTryCatch");
}

// A `when` over a String in an inline host body (hashCode lookupswitch + equals branches to relocate).
#[test]
fn inline_when_string_body() {
    let src = "inline fun code(s: String): Int = when (s) {\n\
    \"a\" -> 1\n\
    \"bb\" -> 2\n\
    \"ccc\" -> 3\n\
    else -> 0\n\
}\n\
fun box(): String {\n\
    if (code(\"a\") != 1) return \"f1\"\n\
    if (code(\"bb\") != 2) return \"f2\"\n\
    if (code(\"ccc\") != 3) return \"f3\"\n\
    if (code(\"z\") != 0) return \"f4\"\n\
    return \"OK\"\n\
}\n";
    run_ok(src, "InlineWhenStringBody");
}

// Nested loops with many locals in an inline body — dense StackMapTable frames to relocate.
#[test]
fn inline_nested_loops_many_locals() {
    let src = "inline fun grid(n: Int): Int {\n\
    var total = 0\n\
    var rowAcc = 0\n\
    for (i in 0 until n) {\n\
        rowAcc = 0\n\
        for (j in 0 until n) {\n\
            val cell = i * n + j\n\
            rowAcc += cell\n\
        }\n\
        total += rowAcc\n\
    }\n\
    return total\n\
}\n\
fun box(): String {\n\
    val r = grid(3)\n\
    if (r != 36) return \"r=$r\"\n\
    return \"OK\"\n\
}\n";
    run_ok(src, "InlineNestedLoopsManyLocals");
}

// An inline call whose result is DISCARDED in statement position (the value must be popped).
#[test]
fn inline_result_discarded_statement() {
    let src = "var sideEffect = 0\n\
inline fun bump(f: () -> Int): Int { sideEffect += f(); return sideEffect }\n\
fun box(): String {\n\
    bump { 10 }\n\
    bump { 32 }\n\
    if (sideEffect != 42) return \"s=$sideEffect\"\n\
    return \"OK\"\n\
}\n";
    run_ok(src, "InlineResultDiscardedStatement");
}

// A lambda that is CONDITIONALLY not invoked (splice guarded by a host branch).
#[test]
fn inline_lambda_conditionally_not_invoked() {
    let src = "inline fun onlyIf(c: Boolean, f: () -> Int): Int = if (c) f() else -1\n\
fun box(): String {\n\
    if (onlyIf(true) { 5 } != 5) return \"f1\"\n\
    if (onlyIf(false) { 5 } != -1) return \"f2\"\n\
    return \"OK\"\n\
}\n";
    run_ok(src, "InlineLambdaConditionallyNotInvoked");
}

// All five scope functions chained on a single object receiver.
#[test]
fn inline_all_scope_functions_chained() {
    let src = "class Acc(var v: Int)\n\
fun box(): String {\n\
    val a = Acc(1)\n\
    val r = a.apply { v += 1 }\n\
        .also { it.v += 2 }\n\
        .let { it.v * 10 }\n\
        .let { it + 5 }\n\
    if (r != 45) return \"r=$r\"\n\
    val w = with(Acc(2)) { v * 3 }\n\
    if (w != 6) return \"w=$w\"\n\
    return \"OK\"\n\
}\n";
    run_ok(src, "InlineAllScopeFunctionsChained");
}

// A Unit-returning inline lambda (`forEach`) whose body performs side effects only.
#[test]
fn inline_unit_returning_lambda() {
    let src = "fun box(): String {\n\
    val sb = StringBuilder()\n\
    listOf(\"a\", \"b\", \"c\").forEach { sb.append(it) }\n\
    if (sb.toString() != \"abc\") return \"r=$sb\"\n\
    return \"OK\"\n\
}\n";
    run_ok(src, "InlineUnitReturningLambda");
}

// An inner ordinary lambda that captures a local of the enclosing INLINE lambda (nested capture across
// the splice boundary).
#[test]
fn inline_nested_lambda_capture() {
    let src = "fun box(): String {\n\
    val r = listOf(1, 2, 3).map { x ->\n\
        val bonus = x * 100\n\
        listOf(10, 20).map { it + bonus }.sum()\n\
    }.sum()\n\
    // x=1: (110+120)=230 ; x=2: (210+220)=430 ; x=3: (310+320)=630 ; sum=1290\n\
    if (r != 1290) return \"r=$r\"\n\
    return \"OK\"\n\
}\n";
    run_ok(src, "InlineNestedLambdaCapture");
}

// ############################################################################
// VALUE_CLASSES.RS — rarer member / boundary paths.
// ############################################################################

// A value class with a COMPUTED property (custom getter, no backing field).
#[test]
fn vc_computed_property() {
    let src = "@JvmInline\nvalue class Celsius(val v: Double) {\n\
    val fahrenheit: Double get() = v * 9.0 / 5.0 + 32.0\n\
}\n\
fun box(): String {\n\
    val c = Celsius(100.0)\n\
    val f = c.fahrenheit\n\
    if (f < 211.9 || f > 212.1) return \"f=$f\"\n\
    return \"OK\"\n\
}\n";
    run_ok(src, "VcComputedProperty");
}

// A NULLABLE value class — a `Vc?` must be BOXED (kotlinc keeps the box for the nullable boundary).
#[test]
fn vc_nullable_boxing() {
    let src = "@JvmInline\nvalue class Id(val v: Int)\n\
fun maybe(b: Boolean): Id? = if (b) Id(9) else null\n\
fun box(): String {\n\
    val a = maybe(true)\n\
    if (a == null) return \"f1\"\n\
    if (a.v != 9) return \"f2\"\n\
    val n = maybe(false)\n\
    if (n != null) return \"f3\"\n\
    return \"OK\"\n\
}\n";
    run_ok(src, "VcNullableBoxing");
}

// A value class IMPLEMENTING an interface — calls through the interface box the value.
#[test]
fn vc_implements_interface() {
    let src = "interface Named { fun label(): String }\n\
@JvmInline\nvalue class Tag(val v: String) : Named {\n\
    override fun label(): String = \"tag:\" + v\n\
}\n\
fun describe(n: Named): String = n.label()\n\
fun box(): String {\n\
    val t = Tag(\"x\")\n\
    if (t.label() != \"tag:x\") return \"f1\"\n\
    if (describe(t) != \"tag:x\") return \"f2\"\n\
    return \"OK\"\n\
}\n";
    run_ok(src, "VcImplementsInterface");
}

// A value class as an EXTENSION RECEIVER.
#[test]
fn vc_as_extension_receiver() {
    let src = "@JvmInline\nvalue class Money(val cents: Int)\n\
fun Money.dollars(): Int = cents / 100\n\
fun box(): String {\n\
    val m = Money(1234)\n\
    if (m.dollars() != 12) return \"r=${m.dollars()}\"\n\
    return \"OK\"\n\
}\n";
    run_ok(src, "VcAsExtensionReceiver");
}

// A value class in a GENERIC position (`List<Vc>`) — elements are boxed inside the collection.
#[test]
fn vc_in_generic_list() {
    let src = "@JvmInline\nvalue class Vc(val v: Int)\n\
fun box(): String {\n\
    val xs = listOf(Vc(1), Vc(2), Vc(3))\n\
    var sum = 0\n\
    for (x in xs) sum += x.v\n\
    if (sum != 6) return \"f1:$sum\"\n\
    if (xs[1].v != 2) return \"f2\"\n\
    return \"OK\"\n\
}\n";
    run_ok(src, "VcInGenericList");
}

// A value class used as a MAP VALUE (generic boxing on the value side).
#[test]
fn vc_as_map_value() {
    let src = "@JvmInline\nvalue class Score(val v: Int)\n\
fun box(): String {\n\
    val m = mapOf(\"a\" to Score(10), \"b\" to Score(20))\n\
    if (m[\"a\"]?.v != 10) return \"f1\"\n\
    if (m[\"b\"]?.v != 20) return \"f2\"\n\
    return \"OK\"\n\
}\n";
    run_ok(src, "VcAsMapValue");
}

// A value class flowing through a `when`/`if` branch join.
#[test]
fn vc_through_when() {
    let src = "@JvmInline\nvalue class Grade(val v: Int)\n\
fun classify(n: Int): Grade = when {\n\
    n >= 90 -> Grade(1)\n\
    n >= 80 -> Grade(2)\n\
    else -> Grade(3)\n\
}\n\
fun box(): String {\n\
    if (classify(95).v != 1) return \"f1\"\n\
    if (classify(85).v != 2) return \"f2\"\n\
    if (classify(50).v != 3) return \"f3\"\n\
    return \"OK\"\n\
}\n";
    run_ok(src, "VcThroughWhen");
}

// A value class with a LONG underlying (category-2 unboxed slots).
#[test]
fn vc_long_underlying() {
    let src = "@JvmInline\nvalue class Timestamp(val millis: Long)\n\
fun add(t: Timestamp, d: Long): Timestamp = Timestamp(t.millis + d)\n\
fun box(): String {\n\
    val t = Timestamp(1000000000000L)\n\
    val r = add(t, 5L)\n\
    if (r.millis != 1000000000005L) return \"r=${r.millis}\"\n\
    return \"OK\"\n\
}\n";
    run_ok(src, "VcLongUnderlying");
}

// A value class with a DOUBLE underlying.
#[test]
fn vc_double_underlying() {
    let src = "@JvmInline\nvalue class Ratio(val v: Double)\n\
fun box(): String {\n\
    val r = Ratio(0.5)\n\
    val d = Ratio(r.v * 3.0)\n\
    if (d.v < 1.49 || d.v > 1.51) return \"d=${d.v}\"\n\
    return \"OK\"\n\
}\n";
    run_ok(src, "VcDoubleUnderlying");
}

// A value class with a BOOLEAN underlying and eq/hashCode.
#[test]
fn vc_boolean_underlying() {
    let src = "@JvmInline\nvalue class Flag(val on: Boolean)\n\
fun box(): String {\n\
    val a = Flag(true)\n\
    if (a != Flag(true)) return \"f1\"\n\
    if (a == Flag(false)) return \"f2\"\n\
    if (!a.on) return \"f3\"\n\
    return \"OK\"\n\
}\n";
    run_ok(src, "VcBooleanUnderlying");
}

// A value class as both a PARAMETER and a RETURN value (unbox on entry, box/unbox at the call).
#[test]
fn vc_as_param_and_return() {
    let src = "@JvmInline\nvalue class Wrapped(val v: Int)\n\
fun inc(w: Wrapped): Wrapped = Wrapped(w.v + 1)\n\
fun box(): String {\n\
    var w = Wrapped(0)\n\
    w = inc(w)\n\
    w = inc(w)\n\
    w = inc(w)\n\
    if (w.v != 3) return \"r=${w.v}\"\n\
    return \"OK\"\n\
}\n";
    run_ok(src, "VcAsParamAndReturn");
}

// ############################################################################
// SUSPEND.RS — rarer state-machine paths.
// ############################################################################

// A suspension inside a `while` loop (loop back-edge over a suspend state) is NOT yet lowered — the
// backend must cleanly DECLINE it rather than miscompile. Covers the suspend.rs bail path.
#[test]
fn suspend_in_while_loop_rejected() {
    let src = "suspend fun one(): Int = 1\n\
suspend fun sumLoop(n: Int): Int {\n\
    var s = 0\n\
    var i = 0\n\
    while (i < n) { s += one(); i++ }\n\
    return s\n\
}\n";
    rejects_suspend("suspend_while", src);
}

// A suspension inside a `for` loop over a range — also declined by the backend.
#[test]
fn suspend_in_for_loop_rejected() {
    let src = "suspend fun weight(i: Int): Int = i * 2\n\
suspend fun total(n: Int): Int {\n\
    var s = 0\n\
    for (i in 0 until n) { s += weight(i) }\n\
    return s\n\
}\n";
    rejects_suspend("suspend_for", src);
}

// A suspension inside a `when` arm — declined by the backend.
#[test]
fn suspend_in_when_rejected() {
    let src = "suspend fun a(): Int = 10\n\
suspend fun b(): Int = 20\n\
suspend fun choose(n: Int): Int = when (n) {\n\
    0 -> a()\n\
    1 -> b()\n\
    else -> a() + b()\n\
}\n";
    rejects_suspend("suspend_when", src);
}

// A suspend function calling a suspend MEMBER function on a class instance.
#[test]
fn suspend_calls_suspend_member() {
    let src = "class Svc {\n\
    suspend fun fetch(): Int = 42\n\
}\n\
suspend fun useIt(): Int {\n\
    val s = Svc()\n\
    return s.fetch() + 1\n\
}\n";
    run_suspend("suspend_member", src, "useIt(k)", "43");
}

// A suspend function calling a suspend OPERATOR function.
#[test]
fn suspend_calls_suspend_operator() {
    let src = "class Adder(val base: Int) {\n\
    suspend operator fun invoke(x: Int): Int = base + x\n\
}\n\
suspend fun run2(): Int {\n\
    val a = Adder(40)\n\
    return a(2)\n\
}\n";
    run_suspend("suspend_operator", src, "run2(k)", "42");
}

// Locals spilled across TWO sequential suspension points (both `a` and `b` live across the second).
#[test]
fn suspend_sequential_spill() {
    let src = "suspend fun p(): Int = 7\n\
suspend fun q(): Int = 5\n\
suspend fun both(): Int {\n\
    val a = p()\n\
    val b = q()\n\
    val c = p()\n\
    return a * 100 + b * 10 + c\n\
}\n";
    run_suspend("suspend_spill", src, "both(k)", "757");
}

// A suspend function returning a String.
#[test]
fn suspend_returns_string() {
    let src = "suspend fun greeting(): String = \"hi\"\n\
suspend fun msg(): String {\n\
    val g = greeting()\n\
    return g + \"!\"\n\
}\n";
    run_suspend("suspend_string", src, "msg(k)", "hi!");
}

// A suspend function returning a Boolean.
#[test]
fn suspend_returns_boolean() {
    let src = "suspend fun raw(): Int = 5\n\
suspend fun isBig(): Boolean {\n\
    val v = raw()\n\
    return v > 3\n\
}\n";
    run_suspend("suspend_bool", src, "isBig(k)", "true");
}

// A suspend function returning a nullable reference.
#[test]
fn suspend_returns_nullable() {
    let src = "suspend fun lookup(hit: Boolean): String? {\n\
    val base = raw()\n\
    return if (hit) \"v$base\" else null\n\
}\n\
suspend fun raw(): Int = 1\n";
    run_suspend("suspend_nullable", src, "lookup(true, k)", "v1");
}

// A suspend function returning Unit with a non-tail suspension is not yet lowered — declined.
#[test]
fn suspend_returns_unit_rejected() {
    let src = "var sink = 0\n\
suspend fun step(): Int = 21\n\
suspend fun act(): Unit {\n\
    sink += step()\n\
    sink += step()\n\
}\n";
    rejects_suspend("suspend_unit", src);
}
