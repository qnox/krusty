//! End-to-end "box" coverage aimed at the inline-function bytecode splicer (`src/jvm/inline.rs`),
//! specifically the paths under-exercised by `feature_coverage_o_e2e.rs`: BRANCHY splices (lambda
//! bodies containing loops/when/try — `StackMapTable` relocation, exception-table relocation, switch
//! offset recomputation), multiple lambda params invoked conditionally / in a loop / out of order,
//! large bodies (frame relocation), deeply nested inlining, and captured-variable lambdas. Each test
//! compiles a self-contained Kotlin program whose `fun box(): String` returns "OK", runs it on the
//! JVM, and asserts the result. Only kotlin-stdlib is on the classpath.

mod common;

use std::path::PathBuf;

/// Compile `src` (stem `stem`) against kotlin-stdlib + JDK modules and run its `box()`, asserting
/// "OK". Skips (returns) when the toolchain / stdlib / JDK isn't provisioned so the suite still runs.
fn run_ok(src: &str, stem: &str) {
    let Some(java_home) = common::java_home() else {
        eprintln!("skipping feature_coverage_u_e2e::{stem}: set JAVA_HOME");
        return;
    };
    let Some(stdlib) = common::stdlib_jar() else {
        eprintln!("skipping feature_coverage_u_e2e::{stem}: no kotlin-stdlib jar found");
        return;
    };
    let jdk = PathBuf::from(format!("{java_home}/lib/modules"));
    let Some(out) = common::compile_and_run_box(src, stem, &[stdlib], Some(&jdk)) else {
        return;
    };
    assert_eq!(out, "OK", "{stem} produced wrong box() result");
}

// --- inline lambda body containing a loop with break/continue (branchy splice) --------------------

#[test]
fn inline_lambda_loop_break_continue() {
    let src = "inline fun apply1(f: () -> Int): Int = f()\n\
fun box(): String {\n\
    val r = apply1 {\n\
        var sum = 0\n\
        for (i in 0 until 10) {\n\
            if (i == 3) continue\n\
            if (i == 7) break\n\
            sum += i\n\
        }\n\
        sum\n\
    }\n\
    if (r != 18) return \"r=$r\"\n\
    return \"OK\"\n\
}\n";
    run_ok(src, "InlineLambdaLoopBreakContinue");
}

// --- inline lambda body containing a try/catch (exception-table relocation) -----------------------

#[test]
fn inline_lambda_try_catch() {
    let src = "inline fun run1(f: () -> Int): Int = f()\n\
fun boom(): Int = throw RuntimeException(\"x\")\n\
fun box(): String {\n\
    val r = run1 {\n\
        try {\n\
            boom()\n\
        } catch (e: RuntimeException) {\n\
            42\n\
        }\n\
    }\n\
    if (r != 42) return \"r=$r\"\n\
    return \"OK\"\n\
}\n";
    run_ok(src, "InlineLambdaTryCatch");
}

// --- inline lambda body containing a try/finally --------------------------------------------------

#[test]
fn inline_lambda_try_finally() {
    let src = "inline fun run1(f: () -> Int): Int = f()\n\
fun box(): String {\n\
    var side = 0\n\
    val r = run1 {\n\
        try {\n\
            10\n\
        } finally {\n\
            side = 5\n\
        }\n\
    }\n\
    if (r != 10) return \"r=$r\"\n\
    if (side != 5) return \"side=$side\"\n\
    return \"OK\"\n\
}\n";
    run_ok(src, "InlineLambdaTryFinally");
}

// --- inline lambda body containing a when (switch offset recomputation) ---------------------------

#[test]
fn inline_lambda_when() {
    let src = "inline fun classifyWith(n: Int, f: (Int) -> String): String = f(n)\n\
fun box(): String {\n\
    val out = StringBuilder()\n\
    for (n in 0..5) {\n\
        val s = classifyWith(n) {\n\
            when (it) {\n\
                0 -> \"a\"\n\
                1 -> \"b\"\n\
                2 -> \"c\"\n\
                3 -> \"d\"\n\
                4 -> \"e\"\n\
                else -> \"z\"\n\
            }\n\
        }\n\
        out.append(s)\n\
    }\n\
    if (out.toString() != \"abcdez\") return \"out=$out\"\n\
    return \"OK\"\n\
}\n";
    run_ok(src, "InlineLambdaWhen");
}

// --- inline lambda body containing nested lambdas -------------------------------------------------

#[test]
fn inline_lambda_nested_lambdas() {
    let src = "inline fun compute(f: () -> Int): Int = f()\n\
fun box(): String {\n\
    val r = compute {\n\
        val nums = listOf(1, 2, 3, 4)\n\
        nums.map { it * 2 }.filter { it > 3 }.sumOf { it }\n\
    }\n\
    if (r != 18) return \"r=$r\"\n\
    return \"OK\"\n\
}\n";
    run_ok(src, "InlineLambdaNestedLambdas");
}

// --- inline lambda doing an early (non-local) return from inside a loop ----------------------------

#[test]
fn inline_lambda_nonlocal_return_from_loop() {
    let src = "inline fun eachChar(s: String, f: (Char) -> Unit) {\n\
    for (c in s) f(c)\n\
}\n\
fun firstDigit(s: String): Char {\n\
    eachChar(s) {\n\
        for (k in 0..0) {\n\
            if (it in '0'..'9') return it\n\
        }\n\
    }\n\
    return '?'\n\
}\n\
fun box(): String {\n\
    if (firstDigit(\"ab3cd\") != '3') return \"a\"\n\
    if (firstDigit(\"abcd\") != '?') return \"b\"\n\
    return \"OK\"\n\
}\n";
    run_ok(src, "InlineLambdaNonlocalReturnFromLoop");
}

// --- inline lambda doing a non-local return from inside a when ------------------------------------

#[test]
fn inline_lambda_nonlocal_return_from_when() {
    let src = "inline fun withVal(n: Int, f: (Int) -> Unit) { f(n) }\n\
fun sign(n: Int): String {\n\
    withVal(n) {\n\
        when {\n\
            it < 0 -> return \"neg\"\n\
            it == 0 -> return \"zero\"\n\
            else -> return \"pos\"\n\
        }\n\
    }\n\
    return \"?\"\n\
}\n\
fun box(): String {\n\
    if (sign(-4) != \"neg\") return \"a\"\n\
    if (sign(0) != \"zero\") return \"b\"\n\
    if (sign(9) != \"pos\") return \"c\"\n\
    return \"OK\"\n\
}\n";
    run_ok(src, "InlineLambdaNonlocalReturnFromWhen");
}

// --- inline lambda containing a throw -------------------------------------------------------------

#[test]
fn inline_lambda_throw() {
    let src = "inline fun tryIt(f: () -> Int): Int {\n\
    return try { f() } catch (e: IllegalStateException) { -1 }\n\
}\n\
fun box(): String {\n\
    val r = tryIt {\n\
        if (2 > 1) throw IllegalStateException(\"boom\")\n\
        7\n\
    }\n\
    if (r != -1) return \"r=$r\"\n\
    return \"OK\"\n\
}\n";
    run_ok(src, "InlineLambdaThrow");
}

// --- inline fun with MULTIPLE lambda params invoked conditionally / in different orders -----------

#[test]
fn inline_multiple_lambdas_conditional() {
    let src = "inline fun pick(cond: Boolean, a: () -> Int, b: () -> Int): Int =\n\
    if (cond) a() else b()\n\
fun box(): String {\n\
    val x = pick(true, { 1 }, { 2 })\n\
    val y = pick(false, { 1 }, { 2 })\n\
    if (x != 1) return \"x=$x\"\n\
    if (y != 2) return \"y=$y\"\n\
    return \"OK\"\n\
}\n";
    run_ok(src, "InlineMultipleLambdasConditional");
}

// --- inline fun with multiple lambda params invoked in a loop -------------------------------------

#[test]
fn inline_multiple_lambdas_in_loop() {
    let src = "inline fun build(n: Int, gen: (Int) -> Int, acc: (Int, Int) -> Int): Int {\n\
    var r = 0\n\
    for (i in 0 until n) {\n\
        r = acc(r, gen(i))\n\
    }\n\
    return r\n\
}\n\
fun box(): String {\n\
    val r = build(5, { it * it }, { a, b -> a + b })\n\
    if (r != 30) return \"r=$r\"\n\
    return \"OK\"\n\
}\n";
    run_ok(src, "InlineMultipleLambdasInLoop");
}

// --- inline fun with both a lambda param AND regular params, control flow between them -------------

#[test]
fn inline_lambda_and_regular_params_control_flow() {
    let src = "inline fun clampedApply(x: Int, lo: Int, hi: Int, f: (Int) -> Int): Int {\n\
    val base = when {\n\
        x < lo -> lo\n\
        x > hi -> hi\n\
        else -> x\n\
    }\n\
    val mapped = f(base)\n\
    return if (mapped < 0) 0 else mapped\n\
}\n\
fun box(): String {\n\
    val a = clampedApply(50, 0, 10) { it * 2 }\n\
    val b = clampedApply(-9, 0, 10) { it - 100 }\n\
    if (a != 20) return \"a=$a\"\n\
    if (b != 0) return \"b=$b\"\n\
    return \"OK\"\n\
}\n";
    run_ok(src, "InlineLambdaAndRegularParamsControlFlow");
}

// --- inline fun with a large body (many locals) so frame relocation matters -----------------------

#[test]
fn inline_large_body_many_locals() {
    let src = "inline fun bigCompute(seed: Int, f: (Int) -> Int): Int {\n\
    val a = seed + 1\n\
    val b = a * 2\n\
    val c = b - 3\n\
    val d = c + a\n\
    val e = d * 2\n\
    var acc = 0\n\
    for (i in 0 until 4) {\n\
        val local = i * i + a - b + c\n\
        acc += local\n\
        if (acc > 1000) break\n\
    }\n\
    val mapped = f(acc + e)\n\
    return a + b + c + d + e + acc + mapped\n\
}\n\
fun box(): String {\n\
    val r = bigCompute(10) { it + 1 }\n\
    val a = 11; val b = 22; val c = 19; val d = 30; val e = 60\n\
    var acc = 0\n\
    for (i in 0 until 4) { acc += i * i + a - b + c }\n\
    val expected = a + b + c + d + e + acc + (acc + e + 1)\n\
    if (r != expected) return \"r=$r exp=$expected\"\n\
    return \"OK\"\n\
}\n";
    run_ok(src, "InlineLargeBodyManyLocals");
}

// --- deeply nested inline calls (3+ levels) ------------------------------------------------------

#[test]
fn inline_deeply_nested() {
    let src = "inline fun l1(x: Int, f: (Int) -> Int): Int = f(x) + 1\n\
inline fun l2(x: Int, f: (Int) -> Int): Int = l1(x, f) + 10\n\
inline fun l3(x: Int, f: (Int) -> Int): Int = l2(x, f) + 100\n\
inline fun l4(x: Int, f: (Int) -> Int): Int = l3(x, f) + 1000\n\
fun box(): String {\n\
    val r = l4(2) { it * 3 }\n\
    if (r != 1117) return \"r=$r\"\n\
    return \"OK\"\n\
}\n";
    run_ok(src, "InlineDeeplyNested");
}

// --- inline lambda capturing multiple variables (mutated var, val, this) --------------------------

#[test]
fn inline_lambda_captures_multiple() {
    let src = "class Accum(val base: Int) {\n\
    var total = 0\n\
    inline fun each(xs: IntArray, f: (Int) -> Unit) { for (x in xs) f(x) }\n\
    fun sumWith(offset: Int, xs: IntArray): Int {\n\
        val factor = 2\n\
        each(xs) { total += it * factor + base + offset }\n\
        return total\n\
    }\n\
}\n\
fun box(): String {\n\
    val r = Accum(100).sumWith(1, intArrayOf(1, 2, 3))\n\
    if (r != 315) return \"r=$r\"\n\
    return \"OK\"\n\
}\n";
    run_ok(src, "InlineLambdaCapturesMultiple");
}

// --- stdlib collection HOFs chained deeply (splice of map/filter/fold/any/all/none/count) ---------

#[test]
fn stdlib_collection_hofs_chained() {
    let src = "fun box(): String {\n\
    val xs = listOf(1, 2, 3, 4, 5, 6, 7, 8)\n\
    val s = xs.filter { it % 2 == 0 }.map { it * it }.fold(0) { a, b -> a + b }\n\
    if (s != 120) return \"s=$s\"\n\
    if (!xs.any { it > 7 }) return \"any\"\n\
    if (xs.all { it > 7 }) return \"all\"\n\
    if (xs.none { it > 7 }) return \"none\"\n\
    val c = xs.count { it % 3 == 0 }\n\
    if (c != 2) return \"c=$c\"\n\
    return \"OK\"\n\
}\n";
    run_ok(src, "StdlibCollectionHofsChained");
}

// --- stdlib forEach / associate / scope-function chain --------------------------------------------

#[test]
fn stdlib_foreach_associate_chain() {
    let src = "fun box(): String {\n\
    var sum = 0\n\
    listOf(1, 2, 3, 4).forEach { sum += it }\n\
    if (sum != 10) return \"sum=$sum\"\n\
    val m = listOf(1, 2, 3).associate { it to it * it }\n\
    if (m[2] != 4) return \"assoc=${m[2]}\"\n\
    val r = listOf(10, 20, 30)\n\
        .filter { it >= 20 }\n\
        .map { it / 10 }\n\
        .let { it.sum() }\n\
        .also { require(it == 5) }\n\
    if (r != 5) return \"r=$r\"\n\
    return \"OK\"\n\
}\n";
    run_ok(src, "StdlibForeachAssociateChain");
}

// --- inline fun returning early BEFORE invoking the lambda ----------------------------------------

#[test]
fn inline_early_return_before_lambda() {
    let src = "inline fun guarded(ok: Boolean, f: () -> Int): Int {\n\
    if (!ok) return -1\n\
    return f() + 1\n\
}\n\
fun box(): String {\n\
    val a = guarded(false) { 100 }\n\
    val b = guarded(true) { 41 }\n\
    if (a != -1) return \"a=$a\"\n\
    if (b != 42) return \"b=$b\"\n\
    return \"OK\"\n\
}\n";
    run_ok(src, "InlineEarlyReturnBeforeLambda");
}

// --- crossinline lambda invoked from a nested lambda ----------------------------------------------

#[test]
fn inline_crossinline_from_nested_lambda() {
    let src = "inline fun runAll(crossinline f: (Int) -> Int): Int {\n\
    val g = { x: Int -> f(x) + f(x + 1) }\n\
    return g(10)\n\
}\n\
fun box(): String {\n\
    val r = runAll { it * 2 }\n\
    if (r != 42) return \"r=$r\"\n\
    return \"OK\"\n\
}\n";
    run_ok(src, "InlineCrossinlineFromNestedLambda");
}

// --- noinline lambda passed onward and invoked later ----------------------------------------------

#[test]
fn inline_noinline_passed_onward() {
    let src = "fun later(f: () -> Int): Int = f() * 2\n\
inline fun forward(noinline f: () -> Int, g: (Int) -> Int): Int {\n\
    val n = later(f)\n\
    return g(n)\n\
}\n\
fun box(): String {\n\
    val r = forward({ 21 }) { it + 0 }\n\
    if (r != 42) return \"r=$r\"\n\
    return \"OK\"\n\
}\n";
    run_ok(src, "InlineNoinlinePassedOnward");
}
