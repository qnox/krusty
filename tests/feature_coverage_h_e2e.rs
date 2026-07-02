//! End-to-end "box" coverage targeting `src/ir_lower.rs` lowering and `src/jvm/ir_emit.rs` emit
//! edge cases: Long/Double arithmetic and numeric promotion, arrays (including 2D and primitive
//! arrays), nested generics, smart-cast chains, `when` supertype unification, string building,
//! `tailrec`, elvis / safe-call chains, non-local returns and bit-ops combined with arithmetic.
//!
//! Each test compiles a self-contained program with a `box(): String` entry point, runs it on the
//! persistent JVM under verification, and asserts the returned value. Requires only kotlin-stdlib.

mod common;

use std::path::PathBuf;

/// Compile `src` (entry `box()`), run it, and assert the result is `"OK"`. Skips (returns) when the
/// JDK / stdlib toolchain isn't provisioned, matching the other `*_e2e` tests.
fn run_ok(src: &str, stem: &str) {
    let Some(java_home) = common::java_home() else {
        return;
    };
    let Some(stdlib) = common::stdlib_jar() else {
        return;
    };
    let jdk = PathBuf::from(format!("{java_home}/lib/modules"));
    let Some(out) = common::compile_and_run_box(src, stem, &[stdlib], Some(&jdk)) else {
        return;
    };
    assert_eq!(out, "OK");
}

#[test]
fn long_double_arithmetic_and_promotion() {
    let src = "fun box(): String {\n\
        val a: Long = 1_000_000_000L\n\
        val b: Long = a * 3L + 7L\n\
        if (b != 3_000_000_007L) return \"f1\"\n\
        val i = 4\n\
        val promoted: Long = i + a\n\
        if (promoted != 1_000_000_004L) return \"f3\"\n\
        val dd = i * 1.5\n\
        if (dd != 6.0) return \"f4\"\n\
        return \"OK\"\n\
    }\n";
    run_ok(src, "LongDoublePromo");
}

#[test]
fn long_comparisons_and_double_nan() {
    let src = "fun box(): String {\n\
        val x: Long = 9_000_000_000L\n\
        val y: Long = 8_999_999_999L\n\
        if (!(x > y)) return \"f1\"\n\
        if (!(y < x)) return \"f2\"\n\
        if (x <= y) return \"f3\"\n\
        val nan = Double.NaN\n\
        if (nan == nan) return \"f4\"\n\
        if (!(nan != nan)) return \"f5\"\n\
        val inf = Double.POSITIVE_INFINITY\n\
        if (!(inf > 1.0e300)) return \"f6\"\n\
        return \"OK\"\n\
    }\n";
    run_ok(src, "LongCmpNaN");
}

#[test]
fn arrays_arrayof_and_iteration() {
    let src = "fun box(): String {\n\
        val a: Array<Int> = arrayOf(1, 2, 3, 4)\n\
        if (a.size != 4) return \"f1\"\n\
        var sum = 0\n\
        for (x in a) sum += x\n\
        if (sum != 10) return \"f2\"\n\
        if (a[2] != 3) return \"f3\"\n\
        a[2] = 30\n\
        if (a[2] != 30) return \"f4\"\n\
        return \"OK\"\n\
    }\n";
    run_ok(src, "ArraysArrayOf");
}

#[test]
fn two_dimensional_arrays() {
    let src = "fun box(): String {\n\
        val grid: Array<IntArray> = Array(3) { i -> IntArray(3) { j -> i * 3 + j } }\n\
        var sum = 0\n\
        for (row in grid) for (v in row) sum += v\n\
        if (sum != 36) return \"f1\"\n\
        if (grid[1][2] != 5) return \"f2\"\n\
        grid[0][0] = 100\n\
        if (grid[0][0] != 100) return \"f3\"\n\
        return \"OK\"\n\
    }\n";
    run_ok(src, "TwoDimArrays");
}

#[test]
fn primitive_arrays_read_write() {
    let src = "fun box(): String {\n\
        val ia = IntArray(4)\n\
        ia[0] = 5; ia[1] = 10; ia[2] = 15; ia[3] = 20\n\
        var s = 0\n\
        for (i in 0 until ia.size) s += ia[i]\n\
        if (s != 50) return \"f1\"\n\
        val da = DoubleArray(3) { it.toDouble() * 1.5 }\n\
        if (da[2] != 3.0) return \"f2\"\n\
        da[1] = 9.0\n\
        if (da[1] != 9.0) return \"f3\"\n\
        return \"OK\"\n\
    }\n";
    run_ok(src, "PrimArraysRW");
}

#[test]
fn nested_generics_box_and_list() {
    let src = "class Box<T>(val v: T)\n\
    fun box(): String {\n\
        val bb: Box<Box<Int>> = Box(Box(42))\n\
        if (bb.v.v != 42) return \"f1\"\n\
        val ll: List<List<Int>> = listOf(listOf(1, 2), listOf(3, 4))\n\
        var sum = 0\n\
        for (inner in ll) for (x in inner) sum += x\n\
        if (sum != 10) return \"f2\"\n\
        if (ll[1][0] != 3) return \"f3\"\n\
        return \"OK\"\n\
    }\n";
    run_ok(src, "NestedGenerics");
}

#[test]
fn generic_function_returning_generic() {
    let src = "fun <T> wrap(x: T): List<T> = listOf(x)\n\
    fun <T> firstOf(xs: List<T>): T = xs[0]\n\
    fun box(): String {\n\
        val w = wrap(wrap(7))\n\
        if (firstOf(firstOf(w)) != 7) return \"f1\"\n\
        val s = firstOf(wrap(\"hi\"))\n\
        if (s != \"hi\") return \"f2\"\n\
        return \"OK\"\n\
    }\n";
    run_ok(src, "GenericRetGeneric");
}

#[test]
fn smart_cast_chains() {
    let src = "fun describe(a: Any): Int {\n\
        if (a is String) return a.length\n\
        if (a is Int && a > 0) return a * 2\n\
        return -1\n\
    }\n\
    fun useNullable(s: String?): Int {\n\
        if (s is String) return s.length\n\
        return 0\n\
    }\n\
    fun box(): String {\n\
        if (describe(\"hello\") != 5) return \"f1\"\n\
        if (describe(21) != 42) return \"f2\"\n\
        if (describe(3.14) != -1) return \"f3\"\n\
        if (useNullable(\"abcd\") != 4) return \"f4\"\n\
        if (useNullable(null) != 0) return \"f5\"\n\
        return \"OK\"\n\
    }\n";
    run_ok(src, "SmartCastChains");
}

#[test]
fn when_supertype_unification() {
    let src = "open class Animal(val name: String)\n\
    class Cat : Animal(\"cat\")\n\
    class Dog : Animal(\"dog\")\n\
    fun pick(n: Int): Animal = when (n) {\n\
        0 -> Cat()\n\
        1 -> Dog()\n\
        else -> Animal(\"other\")\n\
    }\n\
    fun classify(a: Any): String = when {\n\
        a is Int && a > 10 -> \"big\"\n\
        a is Int -> \"small\"\n\
        a is String -> \"str\"\n\
        else -> \"?\"\n\
    }\n\
    fun box(): String {\n\
        if (pick(0).name != \"cat\") return \"f1\"\n\
        if (pick(1).name != \"dog\") return \"f2\"\n\
        if (pick(9).name != \"other\") return \"f3\"\n\
        if (classify(50) != \"big\") return \"f4\"\n\
        if (classify(3) != \"small\") return \"f5\"\n\
        if (classify(\"x\") != \"str\") return \"f6\"\n\
        return \"OK\"\n\
    }\n";
    run_ok(src, "WhenSupertype");
}

#[test]
fn string_building_loop_and_stringbuilder() {
    let src = "fun box(): String {\n\
        var s = \"\"\n\
        for (i in 1..3) s += \"a\" + i\n\
        if (s != \"a1a2a3\") return \"f1\"\n\
        val sb = StringBuilder()\n\
        for (i in 0 until 3) sb.append(\"x\").append(i)\n\
        if (sb.toString() != \"x0x1x2\") return \"f2\"\n\
        val p = \"abc\"\n\
        val q = \"a\" + \"b\" + \"c\"\n\
        if (p != q) return \"f3\"\n\
        if (p == \"abd\") return \"f4\"\n\
        return \"OK\"\n\
    }\n";
    run_ok(src, "StringBuild");
}

#[test]
fn tailrec_function() {
    let src = "tailrec fun sumTo(n: Int, acc: Long): Long =\n\
        if (n == 0) acc else sumTo(n - 1, acc + n)\n\
    tailrec fun factMod(n: Int, acc: Long): Long =\n\
        if (n <= 1) acc else factMod(n - 1, acc * n % 1000000007L)\n\
    fun box(): String {\n\
        if (sumTo(1_000_000, 0L) != 500000500000L) return \"f1\"\n\
        if (factMod(20, 1L) != 146326063L) return \"f2\"\n\
        return \"OK\"\n\
    }\n";
    run_ok(src, "TailRec");
}

#[test]
fn elvis_and_safe_call_chains() {
    let src = "class A(val b: B?)\n\
    class B(val c: String?)\n\
    fun need(s: String?): String = s ?: throw IllegalStateException(\"null\")\n\
    fun early(s: String?): Int {\n\
        val v = s ?: return -1\n\
        return v.length\n\
    }\n\
    fun box(): String {\n\
        val full = A(B(\"deep\"))\n\
        if (full.b?.c?.length != 4) return \"f1\"\n\
        val empty = A(null)\n\
        if (empty.b?.c?.length != null) return \"f2\"\n\
        if (need(\"hi\") != \"hi\") return \"f3\"\n\
        var threw = false\n\
        try { need(null) } catch (e: IllegalStateException) { threw = true }\n\
        if (!threw) return \"f4\"\n\
        if (early(null) != -1) return \"f5\"\n\
        if (early(\"abc\") != 3) return \"f6\"\n\
        return \"OK\"\n\
    }\n";
    run_ok(src, "ElvisSafeCall");
}

#[test]
fn non_local_return_and_foreach() {
    let src = "fun firstEven(xs: List<Int>): Int? {\n\
        xs.forEach { if (it % 2 == 0) return it }\n\
        return null\n\
    }\n\
    fun box(): String {\n\
        if (firstEven(listOf(1, 3, 4, 6)) != 4) return \"f1\"\n\
        if (firstEven(listOf(1, 3, 5)) != null) return \"f2\"\n\
        var sum = 0\n\
        listOf(1, 2, 3, 4).forEach { sum += it }\n\
        if (sum != 10) return \"f3\"\n\
        return \"OK\"\n\
    }\n";
    run_ok(src, "NonLocalReturn");
}

#[test]
fn bit_ops_with_arithmetic() {
    let src = "fun box(): String {\n\
        val a = 0b1100\n\
        val b = 0b1010\n\
        val r = (a and b) or ((a xor b) shl 1) + 1\n\
        if (r != ((12 and 10) or (((12 xor 10) shl 1) + 1))) return \"f1\"\n\
        val x = 5L\n\
        val y = (x shl 32) or 7L\n\
        if (y != 21474836487L) return \"f2\"\n\
        val z = (255 and 0xF0) shr 4\n\
        if (z != 15) return \"f3\"\n\
        val n = (1 shl 10) - 1\n\
        if (n.inv() and 0xFF != 0) return \"f4\"\n\
        return \"OK\"\n\
    }\n";
    run_ok(src, "BitOpsArith");
}
