//! Deep end-to-end "box" coverage for the inline-function bytecode splicer (`src/jvm/inline.rs`),
//! the worst-covered large file. These scenarios deliberately hit branches under-exercised by
//! `feature_coverage_o_e2e.rs` / `feature_coverage_u_e2e.rs`:
//!
//!   * WIDE slots — `long`/`double`/`float` parameters, locals and returns force `param_store_ops`
//!     (`lstore`/`dstore`), `shift_locals` re-encoding, `set_slot`/`collapse_slots` category-2
//!     handling, `ret_vtype` (`Long`/`Double`/`Float`), and the `lcmp`/`fcmp`/`dcmp`/`num_convert`
//!     arms of the operand-stack simulation (`host_state_at`).
//!   * SWITCH bodies — a `when (Int)` in an inline host body compiles to `tableswitch`/`lookupswitch`,
//!     exercising `insn_offsets_at` padding, `assemble_at` switch re-encoding, and the switch arms of
//!     the disassembler / host simulation.
//!   * EXCEPTION-TABLE relocation on the HOST body (`synchronized`, a host-body `try`/`catch`/`finally`),
//!     hitting the `body.handlers` relocation loop.
//!   * Non-trailing returns in the host body (the `made_goto` / join path), deeply-nested inlining
//!     (5 levels), default lambda params, `noinline` lambdas stored then invoked later, reified funs
//!     returning a `T` value, and diverse stdlib collection chains with wide accumulators.
//!
//! Each test compiles a self-contained Kotlin program whose `fun box(): String` returns "OK", runs it
//! on the JVM, and asserts the result. Only kotlin-stdlib is on the classpath.

mod common;

use std::path::PathBuf;

/// Compile `src` (stem `stem`) against kotlin-stdlib + JDK modules and run its `box()`, asserting
/// "OK". Skips (returns) when the toolchain / stdlib / JDK isn't provisioned so the suite still runs.
fn run_ok(src: &str, stem: &str) {
    let Some(java_home) = common::java_home() else {
        eprintln!("skipping inline_deep_coverage_e2e::{stem}: set JAVA_HOME");
        return;
    };
    let Some(stdlib) = common::stdlib_jar() else {
        eprintln!("skipping inline_deep_coverage_e2e::{stem}: no kotlin-stdlib jar found");
        return;
    };
    let jdk = PathBuf::from(format!("{java_home}/lib/modules"));
    let Some(out) = common::compile_and_run_box(src, stem, &[stdlib], Some(&jdk)) else {
        return;
    };
    assert_eq!(out, "OK", "{stem} produced wrong box() result");
}

// ============================================================================
// WIDE SLOTS — long / double / float in inline bodies (category-2 locals).
// ============================================================================

// A user inline fun with a `long` parameter, a wide local, and a `long` return — exercises
// `param_store_ops` `lstore`, `ret_vtype` Long, and category-2 slot allocation in the splice.
#[test]
fn inline_long_param_local_return() {
    let src = "inline fun scaleL(x: Long, k: Long): Long { val t = x * k; return t + 1L }\n\
fun box(): String {\n\
    val a = scaleL(1000000000L, 3L)\n\
    if (a != 3000000001L) return \"a=$a\"\n\
    return \"OK\"\n\
}\n";
    run_ok(src, "InlineLongParamLocalReturn");
}

// A `double` inline fun with a wide local and `dcmp` comparison in the body.
#[test]
fn inline_double_param_local_return() {
    let src = "inline fun avg(a: Double, b: Double): Double { val s = a + b; return s / 2.0 }\n\
fun box(): String {\n\
    val m = avg(3.0, 4.0)\n\
    if (m < 3.49 || m > 3.51) return \"m=$m\"\n\
    return \"OK\"\n\
}\n";
    run_ok(src, "InlineDoubleParamLocalReturn");
}

// A `float` inline fun — `fstore`/`fload`, `fcmp`, `ret_vtype` Float.
#[test]
fn inline_float_param_return() {
    let src = "inline fun addF(a: Float, b: Float): Float = a + b\n\
fun box(): String {\n\
    val r = addF(1.5f, 2.25f)\n\
    if (r < 3.74f || r > 3.76f) return \"r=$r\"\n\
    return \"OK\"\n\
}\n";
    run_ok(src, "InlineFloatParamReturn");
}

// Mixed wide + narrow parameters so slot allocation must skip a slot for each cat-2 value.
#[test]
fn inline_mixed_wide_narrow_params() {
    let src = "inline fun mix(a: Int, b: Long, c: Int, d: Double): Long {\n\
    return a.toLong() + b + c.toLong() + d.toLong()\n\
}\n\
fun box(): String {\n\
    val r = mix(1, 100L, 2, 7.0)\n\
    if (r != 110L) return \"r=$r\"\n\
    return \"OK\"\n\
}\n";
    run_ok(src, "InlineMixedWideNarrowParams");
}

// A `long` comparison inside the inline body → `lcmp` in `host_state_at`.
#[test]
fn inline_long_compare_body() {
    let src = "inline fun maxL(a: Long, b: Long): Long = if (a > b) a else b\n\
fun box(): String {\n\
    val r = maxL(5L, 9L)\n\
    if (r != 9L) return \"r=$r\"\n\
    if (maxL(20L, 2L) != 20L) return \"f2\"\n\
    return \"OK\"\n\
}\n";
    run_ok(src, "InlineLongCompareBody");
}

// ============================================================================
// SWITCH bodies — when(Int) → tableswitch / lookupswitch inside the host body.
// ============================================================================

// A dense `when (Int)` compiles to `tableswitch` — exercises switch padding / re-encoding in the host.
#[test]
fn inline_when_tableswitch_body() {
    let src = "inline fun classify(n: Int): Int = when (n) {\n\
    0 -> 100\n\
    1 -> 101\n\
    2 -> 102\n\
    3 -> 103\n\
    else -> -1\n\
}\n\
fun box(): String {\n\
    if (classify(0) != 100) return \"f0\"\n\
    if (classify(2) != 102) return \"f2\"\n\
    if (classify(9) != -1) return \"f9\"\n\
    return \"OK\"\n\
}\n";
    run_ok(src, "InlineWhenTableswitchBody");
}

// A sparse `when (Int)` compiles to `lookupswitch`.
#[test]
fn inline_when_lookupswitch_body() {
    let src = "inline fun sparse(n: Int): Int = when (n) {\n\
    1 -> 11\n\
    100 -> 22\n\
    10000 -> 33\n\
    else -> 0\n\
}\n\
fun box(): String {\n\
    if (sparse(1) != 11) return \"f1\"\n\
    if (sparse(100) != 22) return \"f100\"\n\
    if (sparse(10000) != 33) return \"f10000\"\n\
    if (sparse(5) != 0) return \"f5\"\n\
    return \"OK\"\n\
}\n";
    run_ok(src, "InlineWhenLookupswitchBody");
}

// ============================================================================
// HOST-BODY branches / early returns (the made_goto + join path).
// ============================================================================

// An inline fun whose OWN body has a non-trailing return — redirected to a `goto` join.
#[test]
fn inline_early_return_in_body() {
    let src = "inline fun clamp(x: Int, lo: Int, hi: Int): Int {\n\
    if (x < lo) return lo\n\
    if (x > hi) return hi\n\
    return x\n\
}\n\
fun box(): String {\n\
    if (clamp(-5, 0, 10) != 0) return \"f1\"\n\
    if (clamp(15, 0, 10) != 10) return \"f2\"\n\
    if (clamp(7, 0, 10) != 7) return \"f3\"\n\
    return \"OK\"\n\
}\n";
    run_ok(src, "InlineEarlyReturnInBody");
}

// An inline fun body with a `for` loop (iinc / branch back-edge, StackMapTable frames).
#[test]
fn inline_loop_in_body() {
    let src = "inline fun sumTo(n: Int): Int {\n\
    var s = 0\n\
    for (i in 0 until n) s += i\n\
    return s\n\
}\n\
fun box(): String {\n\
    val r = sumTo(5)\n\
    if (r != 10) return \"r=$r\"\n\
    return \"OK\"\n\
}\n";
    run_ok(src, "InlineLoopInBody");
}

// ============================================================================
// EXCEPTION-TABLE relocation on the HOST body.
// ============================================================================

// `synchronized(lock) { ... }` is a stdlib inline fun whose body wraps the lambda in monitorenter /
// try / finally / monitorexit — exercises host-body exception-table relocation + monitor ops.
#[test]
fn inline_synchronized_host_handlers() {
    let src = "fun box(): String {\n\
    val lock = Any()\n\
    var acc = 0\n\
    val r = synchronized(lock) {\n\
        acc += 21\n\
        acc + 21\n\
    }\n\
    if (r != 42) return \"r=$r\"\n\
    if (acc != 21) return \"acc=$acc\"\n\
    return \"OK\"\n\
}\n";
    run_ok(src, "InlineSynchronizedHostHandlers");
}

// A user inline fun whose OWN body has a try/catch/finally (host handler relocation, no lambda).
#[test]
fn inline_try_catch_finally_in_body() {
    let src = "inline fun safeDiv(a: Int, b: Int): Int {\n\
    return try {\n\
        a / b\n\
    } catch (e: ArithmeticException) {\n\
        -1\n\
    } finally {\n\
        // side-effect free finally still forces a handler entry\n\
    }\n\
}\n\
fun box(): String {\n\
    if (safeDiv(10, 2) != 5) return \"f1\"\n\
    if (safeDiv(10, 0) != -1) return \"f2\"\n\
    return \"OK\"\n\
}\n";
    run_ok(src, "InlineTryCatchFinallyInBody");
}

// ============================================================================
// checkcast / instanceof / new / anewarray in the host body.
// ============================================================================

// A non-reified inline fun body using `is`/`as` (checkcast + instanceof relocation).
#[test]
fn inline_is_as_in_body() {
    let src = "inline fun lenOf(x: Any): Int = if (x is String) (x as String).length else -1\n\
fun box(): String {\n\
    if (lenOf(\"hello\") != 5) return \"f1\"\n\
    if (lenOf(42) != -1) return \"f2\"\n\
    return \"OK\"\n\
}\n";
    run_ok(src, "InlineIsAsInBody");
}

// An inline fun body that constructs an object and reads it back (new / invokespecial relocation).
#[test]
fn inline_construct_in_body() {
    let src =
        "inline fun wrap(s: String): String = StringBuilder().append(s).append(\"!\").toString()\n\
fun box(): String {\n\
    if (wrap(\"hi\") != \"hi!\") return wrap(\"hi\")\n\
    return \"OK\"\n\
}\n";
    run_ok(src, "InlineConstructInBody");
}

// An inline fun body constructing an array (anewarray / array store relocation).
#[test]
fn inline_array_in_body() {
    let src = "inline fun pair(a: String, b: String): Int = arrayOf(a, b).size\n\
fun box(): String {\n\
    if (pair(\"x\", \"y\") != 2) return \"f\"\n\
    return \"OK\"\n\
}\n";
    run_ok(src, "InlineArrayInBody");
}

// An inline fun body using String constants / ldc (string relocation).
#[test]
fn inline_string_const_in_body() {
    let src = "inline fun greet(name: String): String = \"Hello, \" + name + \"!\"\n\
fun box(): String {\n\
    if (greet(\"Kt\") != \"Hello, Kt!\") return greet(\"Kt\")\n\
    return \"OK\"\n\
}\n";
    run_ok(src, "InlineStringConstInBody");
}

// ============================================================================
// LAMBDA shapes not covered elsewhere.
// ============================================================================

// A lambda returning a `long` — wide `join_stack` at the invoke result.
#[test]
fn inline_lambda_returns_long() {
    let src = "inline fun computeL(f: () -> Long): Long = f() + 1L\n\
fun box(): String {\n\
    val r = computeL { 100L * 3L }\n\
    if (r != 301L) return \"r=$r\"\n\
    return \"OK\"\n\
}\n";
    run_ok(src, "InlineLambdaReturnsLong");
}

// A 3-argument lambda (Function3.invoke) — N-ary lambda argument splicing where the aload is not
// adjacent to the invoke.
#[test]
fn inline_three_arg_lambda() {
    let src =
        "inline fun apply3(a: Int, b: Int, c: Int, f: (Int, Int, Int) -> Int): Int = f(a, b, c)\n\
fun box(): String {\n\
    val r = apply3(2, 3, 4) { x, y, z -> x * 100 + y * 10 + z }\n\
    if (r != 234) return \"r=$r\"\n\
    return \"OK\"\n\
}\n";
    run_ok(src, "InlineThreeArgLambda");
}

// A lambda invoked multiple times AND in a loop within a single inline host.
#[test]
fn inline_lambda_invoked_in_loop_and_after() {
    let src = "inline fun times(n: Int, f: (Int) -> Int): Int {\n\
    var s = 0\n\
    for (i in 0 until n) s += f(i)\n\
    return s\n\
}\n\
fun box(): String {\n\
    val r = times(4) { it * it }\n\
    if (r != 14) return \"r=$r\"\n\
    return \"OK\"\n\
}\n";
    run_ok(src, "InlineLambdaInvokedInLoopAndAfter");
}

// A lambda param with a default value (`f: () -> Int = { 0 }`), called with and without it.
#[test]
fn inline_default_lambda_param() {
    let src = "inline fun getOr(x: Int, f: () -> Int = { 0 }): Int = if (x > 0) x else f()\n\
fun box(): String {\n\
    if (getOr(5) != 5) return \"f1\"\n\
    if (getOr(-1) { 99 } != 99) return \"f2\"\n\
    if (getOr(-2) != 0) return \"f3\"\n\
    return \"OK\"\n\
}\n";
    run_ok(src, "InlineDefaultLambdaParam");
}

// A `noinline` lambda stored in a val and invoked later (not spliced — must survive as a real object).
#[test]
fn inline_noinline_stored_then_invoked() {
    let src = "inline fun deferred(noinline f: () -> Int): Int {\n\
    val g = f\n\
    val h = f\n\
    return g() + h()\n\
}\n\
fun box(): String {\n\
    val r = deferred { 21 }\n\
    if (r != 42) return \"r=$r\"\n\
    return \"OK\"\n\
}\n";
    run_ok(src, "InlineNoinlineStoredThenInvoked");
}

// ============================================================================
// DEEP nesting + reified.
// ============================================================================

// Five levels of nested inline calls (inline fun calling inline fun … 5 deep).
#[test]
fn inline_five_levels_deep() {
    let src = "inline fun l1(x: Int): Int = x + 1\n\
inline fun l2(x: Int): Int = l1(x) + 1\n\
inline fun l3(x: Int): Int = l2(x) + 1\n\
inline fun l4(x: Int): Int = l3(x) + 1\n\
inline fun l5(x: Int): Int = l4(x) + 1\n\
fun box(): String {\n\
    val r = l5(10)\n\
    if (r != 15) return \"r=$r\"\n\
    return \"OK\"\n\
}\n";
    run_ok(src, "InlineFiveLevelsDeep");
}

// A non-local return crossing two inline frames (outer inline calls inner inline; the lambda returns
// from the outermost function).
#[test]
fn inline_nonlocal_return_two_frames() {
    let src = "inline fun outer(f: () -> Unit) { inner { f() } }\n\
inline fun inner(g: () -> Unit) { g() }\n\
fun probe(hit: Boolean): Int {\n\
    outer { if (hit) return 7 }\n\
    return -1\n\
}\n\
fun box(): String {\n\
    if (probe(true) != 7) return \"f1\"\n\
    if (probe(false) != -1) return \"f2\"\n\
    return \"OK\"\n\
}\n";
    run_ok(src, "InlineNonlocalReturnTwoFrames");
}

// `forEachIndexed` — a 2-argument inline lambda (Function2.invoke) whose index/element aloads are not
// adjacent to the invoke, spliced inside the host loop.
#[test]
fn inline_foreach_indexed() {
    let src = "fun box(): String {\n\
    val xs = listOf(10, 20, 30)\n\
    var acc = 0\n\
    xs.forEachIndexed { i, v -> acc += i * v }\n\
    if (acc != 80) return \"acc=$acc\"\n\
    return \"OK\"\n\
}\n";
    run_ok(src, "InlineForeachIndexed");
}

// A reified inline fun used to count values of a type — the reified `is T` marker resolves against the
// concrete type argument inside a spliced predicate lambda.
#[test]
fn inline_reified_returns_t() {
    let src = "inline fun <reified T> countOfType(xs: List<Any>): Int = xs.count { it is T }\n\
fun box(): String {\n\
    val xs = listOf(1, \"two\", 3.0, \"four\", \"five\")\n\
    val n = countOfType<String>(xs)\n\
    if (n != 3) return \"n=$n\"\n\
    return \"OK\"\n\
}\n";
    run_ok(src, "InlineReifiedReturnsT");
}

// ============================================================================
// STDLIB inline chains that splice — varied element / return / accumulator types.
// ============================================================================

// `fold` with a Long accumulator — a category-2 accumulator lives in the host loop frame (wide-slot
// relocation of a StackMapTable local).
#[test]
fn stdlib_fold_long_accumulator() {
    let src = "fun box(): String {\n\
    val xs = listOf(1, 2, 3, 4, 5)\n\
    val sum = xs.fold(0L) { acc, x -> acc + x.toLong() }\n\
    if (sum != 15L) return \"sum=$sum\"\n\
    return \"OK\"\n\
}\n";
    run_ok(src, "StdlibFoldLongAccumulator");
}

// A deep map/filter/map chain — many nested branchy splices with intermediate collections.
#[test]
fn stdlib_map_filter_map_chain() {
    let src = "fun box(): String {\n\
    val xs = listOf(1, 2, 3, 4, 5, 6, 7, 8)\n\
    val r = xs.map { it * 2 }\n\
        .filter { it % 4 == 0 }\n\
        .map { it + 1 }\n\
        .sum()\n\
    // doubles: 2,4,6,8,10,12,14,16 ; %4==0: 4,8,12,16 ; +1: 5,9,13,17 ; sum=44\n\
    if (r != 44) return \"r=$r\"\n\
    return \"OK\"\n\
}\n";
    run_ok(src, "StdlibMapFilterMapChain");
}

// any / all / none / count predicates in one chain.
#[test]
fn stdlib_predicate_family() {
    let src = "fun box(): String {\n\
    val xs = listOf(2, 4, 6, 8)\n\
    if (!xs.all { it % 2 == 0 }) return \"all\"\n\
    if (xs.any { it > 100 }) return \"any\"\n\
    if (!xs.none { it < 0 }) return \"none\"\n\
    if (xs.count { it >= 4 } != 3) return \"count\"\n\
    return \"OK\"\n\
}\n";
    run_ok(src, "StdlibPredicateFamily");
}

// sumOf (Int and Long-producing lambda), maxByOrNull / minByOrNull.
#[test]
fn stdlib_sumof_maxby_minby() {
    let src = "fun box(): String {\n\
    val xs = listOf(3, 1, 4, 1, 5, 9)\n\
    val si = xs.sumOf { it }\n\
    if (si != 23) return \"si=$si\"\n\
    val sl = xs.sumOf { it.toLong() * 2L }\n\
    if (sl != 46L) return \"sl=$sl\"\n\
    val mx = xs.maxByOrNull { it }\n\
    if (mx != 9) return \"mx=$mx\"\n\
    val mn = xs.minByOrNull { it }\n\
    if (mn != 1) return \"mn=$mn\"\n\
    return \"OK\"\n\
}\n";
    run_ok(src, "StdlibSumofMaxbyMinby");
}

// associateWith / groupBy — build maps via inline HOFs.
#[test]
fn stdlib_associate_groupby() {
    let src = "fun box(): String {\n\
    val xs = listOf(1, 2, 3, 4)\n\
    val m = xs.associateWith { it * it }\n\
    if (m[3] != 9) return \"m=$m\"\n\
    val g = xs.groupBy { it % 2 }\n\
    if (g[0]?.size != 2) return \"g=$g\"\n\
    return \"OK\"\n\
}\n";
    run_ok(src, "StdlibAssociateGroupby");
}

// sortedBy / filterNot / mapIndexed — additional inline HOFs with index-carrying lambdas.
#[test]
fn stdlib_sortedby_filternot_mapindexed() {
    let src = "fun box(): String {\n\
    val xs = listOf(3, 1, 2)\n\
    val s = xs.sortedBy { it }\n\
    if (s != listOf(1, 2, 3)) return \"s=$s\"\n\
    val fn = xs.filterNot { it == 1 }\n\
    if (fn.size != 2) return \"fn=$fn\"\n\
    val mi = xs.mapIndexed { i, v -> i + v }\n\
    if (mi.sum() != 9) return \"mi=$mi\"\n\
    return \"OK\"\n\
}\n";
    run_ok(src, "StdlibSortedbyFilternotMapindexed");
}

// takeIf whose nullable result is chained through a safe-call `?.let` + elvis — the inline takeIf
// splice feeds a nullable value across the splice boundary.
#[test]
fn stdlib_takeif_scope_chain() {
    let src = "fun box(): String {\n\
    val a = 5.takeIf { it > 3 }?.let { it + 100 } ?: -1\n\
    if (a != 105) return \"a=$a\"\n\
    val b = 2.takeIf { it > 3 }?.let { it + 100 } ?: -1\n\
    if (b != -1) return \"b=$b\"\n\
    return \"OK\"\n\
}\n";
    run_ok(src, "StdlibTakeifScopeChain");
}

// `repeat` accumulating into a Long var (wide caller local across the branchy splice).
#[test]
fn stdlib_repeat_long_accumulator() {
    let src = "fun box(): String {\n\
    var acc = 0L\n\
    repeat(5) { acc += (it + 1).toLong() }\n\
    if (acc != 15L) return \"acc=$acc\"\n\
    return \"OK\"\n\
}\n";
    run_ok(src, "StdlibRepeatLongAccumulator");
}

// scope functions on wide (Double) receivers — let/run/also/apply with a Double.
#[test]
fn stdlib_scope_wide_receiver() {
    let src = "fun box(): String {\n\
    val d = 3.0.let { it * 2.0 }.also { }.run { this + 1.0 }\n\
    if (d < 6.99 || d > 7.01) return \"d=$d\"\n\
    return \"OK\"\n\
}\n";
    run_ok(src, "StdlibScopeWideReceiver");
}
