//! End-to-end "box" coverage for inline-function splicing (`src/jvm/inline.rs`) and the resolver
//! paths that feed it (`src/resolve.rs`). Each test compiles a self-contained Kotlin program whose
//! `fun box(): String` returns "OK", runs it on the JVM, and asserts the result. Only kotlin-stdlib
//! is on the classpath.

mod common;

use std::path::PathBuf;

/// Compile `src` (stem `stem`) against kotlin-stdlib + JDK modules and run its `box()`, asserting
/// "OK". Skips (returns) when the toolchain / stdlib / JDK isn't provisioned so the suite still runs.
fn run_ok(src: &str, stem: &str) {
    let Some(java_home) = common::java_home() else {
        eprintln!("skipping feature_coverage_o_e2e::{stem}: set JAVA_HOME");
        return;
    };
    let Some(stdlib) = common::stdlib_jar() else {
        eprintln!("skipping feature_coverage_o_e2e::{stem}: no kotlin-stdlib jar found");
        return;
    };
    let jdk = PathBuf::from(format!("{java_home}/lib/modules"));
    let Some(out) = common::compile_and_run_box(src, stem, &[stdlib], Some(&jdk)) else {
        return;
    };
    assert_eq!(out, "OK", "{stem} produced wrong box() result");
}

// --- inline fun taking a lambda, called with different lambdas -----------------------------------

#[test]
fn inline_lambda_varied() {
    let src = "inline fun apply2(x: Int, f: (Int) -> Int): Int = f(f(x))\n\
fun box(): String {\n\
    val a = apply2(3) { it + 1 }\n\
    val b = apply2(3) { it * 2 }\n\
    if (a != 5) return \"a=$a\"\n\
    if (b != 12) return \"b=$b\"\n\
    return \"OK\"\n\
}\n";
    run_ok(src, "InlineLambdaVaried");
}

// --- inline fun returning a value produced by the lambda -----------------------------------------

#[test]
fn inline_lambda_returns_value() {
    let src = "inline fun <T> produce(f: () -> T): T = f()\n\
fun box(): String {\n\
    val s: String = produce { \"hi\" }\n\
    val n: Int = produce { 41 + 1 }\n\
    if (s != \"hi\") return \"s=$s\"\n\
    if (n != 42) return \"n=$n\"\n\
    return \"OK\"\n\
}\n";
    run_ok(src, "InlineLambdaReturnsValue");
}

// --- inline fun with TWO lambda params, both invoked ---------------------------------------------

#[test]
fn inline_two_lambdas() {
    let src = "inline fun combine(a: () -> Int, b: () -> Int): Int = a() * 10 + b()\n\
fun box(): String {\n\
    val r = combine({ 4 }, { 2 })\n\
    if (r != 42) return \"r=$r\"\n\
    return \"OK\"\n\
}\n";
    run_ok(src, "InlineTwoLambdas");
}

// --- non-local return from an inline lambda ------------------------------------------------------

#[test]
fn inline_non_local_return() {
    let src = "inline fun forEachInt(xs: IntArray, f: (Int) -> Unit) {\n\
    for (x in xs) f(x)\n\
}\n\
fun firstEven(xs: IntArray): Int {\n\
    forEachInt(xs) { if (it % 2 == 0) return it }\n\
    return -1\n\
}\n\
fun box(): String {\n\
    val r = firstEven(intArrayOf(1, 3, 8, 5))\n\
    if (r != 8) return \"r=$r\"\n\
    if (firstEven(intArrayOf(1, 3, 5)) != -1) return \"none\"\n\
    return \"OK\"\n\
}\n";
    run_ok(src, "InlineNonLocalReturn");
}

// --- crossinline lambda param --------------------------------------------------------------------

#[test]
fn inline_crossinline() {
    let src = "inline fun runTwice(crossinline f: () -> Int): Int {\n\
    val g = { f() + f() }\n\
    return g()\n\
}\n\
fun box(): String {\n\
    val r = runTwice { 21 }\n\
    if (r != 42) return \"r=$r\"\n\
    return \"OK\"\n\
}\n";
    run_ok(src, "InlineCrossinline");
}

// --- noinline lambda param stored / passed on ----------------------------------------------------

#[test]
fn inline_noinline() {
    let src = "fun call(f: () -> Int): Int = f()\n\
inline fun wrap(noinline f: () -> Int): Int {\n\
    val stored: () -> Int = f\n\
    return call(stored)\n\
}\n\
fun box(): String {\n\
    val r = wrap { 42 }\n\
    if (r != 42) return \"r=$r\"\n\
    return \"OK\"\n\
}\n";
    run_ok(src, "InlineNoinline");
}

// --- reified type param: is T / T::class ---------------------------------------------------------

#[test]
fn inline_reified_is() {
    let src = "inline fun <reified T> countOf(xs: List<Any>): Int {\n\
    var n = 0\n\
    for (x in xs) if (x is T) n++\n\
    return n\n\
}\n\
fun box(): String {\n\
    val xs = listOf(1, \"a\", 2, \"b\", 3)\n\
    val ints = countOf<Int>(xs)\n\
    val strs = countOf<String>(xs)\n\
    if (ints != 3) return \"ints=$ints\"\n\
    if (strs != 2) return \"strs=$strs\"\n\
    return \"OK\"\n\
}\n";
    run_ok(src, "InlineReifiedIs");
}

#[test]
fn inline_reified_class_name() {
    let src = "inline fun <reified T> nameOf(): String = T::class.java.simpleName\n\
fun box(): String {\n\
    val n = nameOf<String>()\n\
    if (n != \"String\") return \"n=$n\"\n\
    return \"OK\"\n\
}\n";
    run_ok(src, "InlineReifiedClassName");
}

// --- inline higher-order stdlib usage: let/run/with/apply/also -----------------------------------

#[test]
fn stdlib_scope_functions() {
    let src = "fun box(): String {\n\
    val a = 5.let { it + 1 }\n\
    if (a != 6) return \"let=$a\"\n\
    val b = run { 40 + 2 }\n\
    if (b != 42) return \"run=$b\"\n\
    val c = with(listOf(1, 2, 3)) { size }\n\
    if (c != 3) return \"with=$c\"\n\
    val sb = StringBuilder().apply { append(\"x\"); append(\"y\") }\n\
    if (sb.toString() != \"xy\") return \"apply=$sb\"\n\
    var side = 0\n\
    val d = 7.also { side = it }\n\
    if (d != 7 || side != 7) return \"also=$d/$side\"\n\
    return \"OK\"\n\
}\n";
    run_ok(src, "StdlibScopeFunctions");
}

// --- takeIf / takeUnless -------------------------------------------------------------------------

#[test]
fn stdlib_take_if_unless() {
    let src = "fun box(): String {\n\
    val a = 5.takeIf { it > 3 }\n\
    if (a != 5) return \"takeIf=$a\"\n\
    val b = 2.takeIf { it > 3 }\n\
    if (b != null) return \"takeIfNull=$b\"\n\
    val c = 5.takeUnless { it > 3 }\n\
    if (c != null) return \"takeUnless=$c\"\n\
    val d = 2.takeUnless { it > 3 }\n\
    if (d != 2) return \"takeUnlessVal=$d\"\n\
    return \"OK\"\n\
}\n";
    run_ok(src, "StdlibTakeIfUnless");
}

// --- repeat --------------------------------------------------------------------------------------

#[test]
fn stdlib_repeat() {
    let src = "fun box(): String {\n\
    var sum = 0\n\
    repeat(4) { sum += it }\n\
    if (sum != 6) return \"sum=$sum\"\n\
    return \"OK\"\n\
}\n";
    run_ok(src, "StdlibRepeat");
}

// --- inline extension function on a user type ----------------------------------------------------

#[test]
fn inline_extension_user_type() {
    let src = "class Box(val v: Int)\n\
inline fun Box.twice(f: (Int) -> Int): Int = f(v) + f(v)\n\
fun box(): String {\n\
    val r = Box(10).twice { it + 1 }\n\
    if (r != 22) return \"r=$r\"\n\
    return \"OK\"\n\
}\n";
    run_ok(src, "InlineExtensionUserType");
}

// --- inline extension on Int / String ------------------------------------------------------------

#[test]
fn inline_extension_builtins() {
    let src = "inline fun Int.applyN(n: Int, f: (Int) -> Int): Int {\n\
    var acc = this\n\
    repeat(n) { acc = f(acc) }\n\
    return acc\n\
}\n\
inline fun String.transform(f: (String) -> String): String = f(this)\n\
fun box(): String {\n\
    val a = 1.applyN(3) { it * 2 }\n\
    if (a != 8) return \"a=$a\"\n\
    val b = \"ab\".transform { it + it }\n\
    if (b != \"abab\") return \"b=$b\"\n\
    return \"OK\"\n\
}\n";
    run_ok(src, "InlineExtensionBuiltins");
}

// --- inline fun calling another inline fun (nested inlining) --------------------------------------

#[test]
fn inline_nested() {
    let src = "inline fun once(x: Int, f: (Int) -> Int): Int = f(x)\n\
inline fun thrice(x: Int, f: (Int) -> Int): Int = once(once(once(x, f), f), f)\n\
fun box(): String {\n\
    val r = thrice(2) { it + 3 }\n\
    if (r != 11) return \"r=$r\"\n\
    return \"OK\"\n\
}\n";
    run_ok(src, "InlineNested");
}

// --- inline property getter ----------------------------------------------------------------------

#[test]
fn inline_property_getter() {
    let src = "class Celsius(val v: Int) {\n\
    val doubled: Int inline get() = v * 2\n\
}\n\
fun box(): String {\n\
    val r = Celsius(21).doubled\n\
    if (r != 42) return \"r=$r\"\n\
    return \"OK\"\n\
}\n";
    run_ok(src, "InlinePropertyGetter");
}

// --- run { } returning from the outer function; also/apply chaining ------------------------------

#[test]
fn run_non_local_return() {
    let src = "fun classify(n: Int): String {\n\
    run {\n\
        if (n < 0) return \"neg\"\n\
        if (n == 0) return \"zero\"\n\
    }\n\
    return \"pos\"\n\
}\n\
fun box(): String {\n\
    if (classify(-2) != \"neg\") return \"neg?\"\n\
    if (classify(0) != \"zero\") return \"zero?\"\n\
    if (classify(9) != \"pos\") return \"pos?\"\n\
    return \"OK\"\n\
}\n";
    run_ok(src, "RunNonLocalReturn");
}

#[test]
fn also_apply_chaining() {
    let src = "class Counter {\n\
    var n = 0\n\
    fun inc(): Counter { n++; return this }\n\
}\n\
fun box(): String {\n\
    val log = StringBuilder()\n\
    val c = Counter()\n\
        .apply { inc() }\n\
        .apply { inc() }\n\
        .also { log.append(it.n) }\n\
    if (c.n != 2) return \"n=${c.n}\"\n\
    if (log.toString() != \"2\") return \"log=$log\"\n\
    return \"OK\"\n\
}\n";
    run_ok(src, "AlsoApplyChaining");
}
