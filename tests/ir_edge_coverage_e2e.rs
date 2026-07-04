//! End-to-end box tests aimed at less-common but supported lowering/emit branches in
//! `src/ir_lower.rs` and `src/jvm/ir_emit.rs`: numeric conversions & edge values, bit ops,
//! string ops, exotic control-flow shapes, collection/array operations, and misc language
//! constructs. Each `box()` returns "OK"; compiled in-process and round-tripped on the JVM.

use super::common;

use std::path::PathBuf;

/// Compile `src`, run its `box()`, and assert it returned "OK". Skips (returns) only when the
/// JVM toolchain / stdlib isn't provisioned; a `None` from a provisioned toolchain is a real
/// compile/run failure and panics so regressions surface.
fn ok(src: &str, stem: &str) {
    let Some(java_home) = common::java_home() else {
        eprintln!("skipping {stem}: set JAVA_HOME");
        return;
    };
    let Some(stdlib) = common::stdlib_jar() else {
        eprintln!("skipping {stem}: no kotlin-stdlib jar found");
        return;
    };
    let jdk = PathBuf::from(format!("{java_home}/lib/modules"));
    match common::compile_and_run_box(src, stem, &[stdlib], Some(&jdk)) {
        Some(out) => assert_eq!(out, "OK", "{stem}"),
        None => panic!("{stem}: krusty failed to compile/run the box"),
    }
}

#[test]
fn numeric_conversion_chains() {
    let src = r#"
fun box(): String {
    val i = 300
    if (i.toByte().toInt() != 44) return "b1"
    if (i.toShort().toInt() != 300) return "s1"
    if (i.toLong() != 300L) return "l1"
    if (i.toFloat() != 300.0f) return "f1"
    if (i.toDouble() != 300.0) return "d1"
    if (65.toChar() != 'A') return "c1"
    if ('A'.code != 65) return "c2"
    val d = 3.99
    if (d.toInt() != 3) return "d2"
    if (d.toLong() != 3L) return "d3"
    if ((-3.99).toInt() != -3) return "d4"
    if (300L.toByte().toInt() != 44) return "l2"
    if (1.5f.toInt() != 1) return "f2"
    if (1.0f.toDouble() != 1.0) return "f3"
    if (7L.toInt().toShort().toLong() != 7L) return "chain"
    return "OK"
}
"#;
    ok(src, "NumConv");
}

#[test]
fn integer_edge_values() {
    let src = r#"
fun box(): String {
    if (Int.MAX_VALUE + 1 != Int.MIN_VALUE) return "i1"
    if (Int.MIN_VALUE - 1 != Int.MAX_VALUE) return "i2"
    if (-Int.MIN_VALUE != Int.MIN_VALUE) return "i3"
    if (Long.MAX_VALUE + 1L != Long.MIN_VALUE) return "l1"
    if (-Long.MIN_VALUE != Long.MIN_VALUE) return "l2"
    if (Int.MAX_VALUE.toLong() + 1L != 2147483648L) return "l3"
    if (Int.MIN_VALUE / -1 != Int.MIN_VALUE) return "i4"
    if (Long.MAX_VALUE % 2L != 1L) return "l4"
    return "OK"
}
"#;
    ok(src, "IntEdge");
}

#[test]
fn bit_operations() {
    let src = r#"
fun box(): String {
    val x = 0x0F
    if (x and 0x03 != 0x03) return "a"
    if (x or 0x30 != 0x3F) return "o"
    if (x xor 0xFF != 0xF0) return "x"
    if (x.inv() != -16) return "inv"
    if (1 shl 0 != 1) return "s0"
    if (1 shl 31 != Int.MIN_VALUE) return "s31"
    if (Int.MIN_VALUE ushr 31 != 1) return "u31"
    if (-1 shr 31 != -1) return "r31"
    if (1L shl 63 != Long.MIN_VALUE) return "s63"
    if (Long.MIN_VALUE ushr 63 != 1L) return "u63"
    if (255 shr 0 != 255) return "r0"
    if (1L shl 0 != 1L) return "ls0"
    return "OK"
}
"#;
    ok(src, "BitOps");
}

#[test]
fn long_div_mod() {
    let src = r#"
fun box(): String {
    if (100L / 7L != 14L) return "d1"
    if (100L % 7L != 2L) return "m1"
    if (-100L / 7L != -14L) return "d2"
    if (-100L % 7L != -2L) return "m2"
    if (100L / -7L != -14L) return "d3"
    if (100L % -7L != 2L) return "m3"
    return "OK"
}
"#;
    ok(src, "LongDivMod");
}

#[test]
fn double_special_values() {
    let src = r#"
fun box(): String {
    val nan = Double.NaN
    if (nan == nan) return "n1"
    if (!(nan != nan)) return "n2"
    if (nan.isNaN() != true) return "n3"
    val inf = Double.POSITIVE_INFINITY
    if (!(inf > 1e300)) return "i1"
    if (Double.NEGATIVE_INFINITY >= 0.0) return "i2"
    if (1.0 / 0.0 != inf) return "i3"
    if (!(inf.isInfinite())) return "i4"
    if (0.0 == -0.0 && (0.0).equals(-0.0)) return "z1"
    return "OK"
}
"#;
    ok(src, "DblSpecial");
}

#[test]
fn float_truncation() {
    let src = r#"
fun box(): String {
    val f = 2.9f
    if (f.toInt() != 2) return "t1"
    if ((-2.9f).toInt() != -2) return "t2"
    if (1e10f.toLong() != 10000000000L) return "t3"
    if (3.7.toFloat().toInt() != 3) return "t4"
    return "OK"
}
"#;
    ok(src, "FloatTrunc");
}

#[test]
fn string_comparison_operators() {
    let src = r#"
fun box(): String {
    if ("abc".compareTo("abd") >= 0) return "c1"
    if ("abd".compareTo("abc") <= 0) return "c2"
    if ("abc".compareTo("abc") != 0) return "c3"
    if ("Z".compareTo("a") >= 0) return "c4"
    if ("abc" == "abd") return "c5"
    if ("abc" != "abc") return "c6"
    if (!("abc" == "abc")) return "c7"
    if (!("a" != "b")) return "c8"
    return "OK"
}
"#;
    ok(src, "StrCmp");
}

#[test]
fn string_mutation_and_access() {
    let src = r#"
fun box(): String {
    var s = "ab"
    s += "cd"
    if (s != "abcd") return "a1"
    if (s[1] != 'b') return "a2"
    if (s.substring(1, 3) != "bc") return "a3"
    if (s.replace("b", "X") != "aXcd") return "a4"
    val parts = "a,b,c".split(",")
    if (parts.size != 3 || parts[2] != "c") return "a5"
    if (s.indexOf('c') != 2) return "a7"
    if (s.reversed().toString() != "dcba") return "a8"
    return "OK"
}
"#;
    ok(src, "StrMut");
}

#[test]
fn string_case_and_predicates() {
    let src = r#"
fun box(): String {
    if ("Hello".uppercase() != "HELLO") return "u"
    if ("Hello".lowercase() != "hello") return "l"
    if (!"Hello".startsWith("He")) return "s"
    if (!"Hello".endsWith("lo")) return "e"
    if (!"".isEmpty()) return "em"
    if (!"   ".isBlank()) return "bl"
    if ("7".padStart(3, '0') != "007") return "ps"
    if ("7".padEnd(3, '0') != "700") return "pe"
    if (!"Hello".contains("ell")) return "co"
    if ("Hello".trim().length != 5) return "tr"
    return "OK"
}
"#;
    ok(src, "StrCase");
}

#[test]
fn char_arithmetic() {
    let src = r#"
fun box(): String {
    val c = 'a'
    if (c + 1 != 'b') return "p1"
    if ('z' - 'a' != 25) return "p2"
    if ('c' - 1 != 'b') return "p3"
    var acc = 0
    for (ch in 'a'..'e') acc += ch - 'a'
    if (acc != 10) return "p4"
    if (!'5'.isDigit()) return "p5"
    if (!'A'.isLetter()) return "p6"
    if ('A' + 32 != 'a') return "p7"
    return "OK"
}
"#;
    ok(src, "CharArith");
}

#[test]
fn stringbuilder_operations() {
    let src = r#"
fun box(): String {
    val sb = StringBuilder()
    sb.append("acd")
    sb.insert(1, "b")
    if (sb.toString() != "abcd") return "s1"
    sb.deleteCharAt(0)
    if (sb.toString() != "bcd") return "s2"
    sb.reverse()
    if (sb.toString() != "dcb") return "s3"
    sb.append('x').append(1).append(true)
    if (sb.toString() != "dcbx1true") return "s4"
    if (sb.length != 9) return "s5"
    return "OK"
}
"#;
    ok(src, "StrBuilder");
}

#[test]
fn labeled_break_continue_nested() {
    let src = r#"
fun box(): String {
    var count = 0
    outer@ for (i in 0 until 4) {
        for (j in 0 until 4) {
            if (j == 2) continue@outer
            count++
        }
        count += 100
    }
    if (count != 8) return "c1"
    var found = -1
    loop@ for (i in 0 until 3) {
        for (j in 0 until 3) {
            for (k in 0 until 3) {
                if (i + j + k == 4) { found = i * 100 + j * 10 + k; break@loop }
            }
        }
    }
    if (found != 22) return "c2"
    return "OK"
}
"#;
    ok(src, "LabelBreak");
}

#[test]
fn when_with_ranges_in_and_type() {
    let src = r#"
fun classify(x: Any): String = when (x) {
    is Int -> when (x) {
        in 0..9 -> "digit"
        in 10..99 -> "tens"
        else -> "big"
    }
    is String -> if (x in listOf("a", "b")) "known" else "str"
    else -> "other"
}
fun box(): String {
    if (classify(5) != "digit") return "1"
    if (classify(50) != "tens") return "2"
    if (classify(500) != "big") return "3"
    if (classify("a") != "known") return "4"
    if (classify("z") != "str") return "5"
    if (classify(1.0) != "other") return "6"
    return "OK"
}
"#;
    ok(src, "WhenRange");
}

#[test]
fn when_without_subject() {
    let src = r#"
fun grade(n: Int): String = when {
    n >= 90 -> "A"
    n >= 80 -> "B"
    n >= 70 -> "C"
    else -> "F"
}
fun box(): String {
    if (grade(95) != "A") return "1"
    if (grade(85) != "B") return "2"
    if (grade(72) != "C") return "3"
    if (grade(50) != "F") return "4"
    return "OK"
}
"#;
    ok(src, "WhenNoSubj");
}

#[test]
fn nested_try_in_when_in_for() {
    let src = r#"
fun box(): String {
    var sum = 0
    for (i in 0 until 5) {
        when (i % 3) {
            0 -> {
                try {
                    if (i == 3) throw RuntimeException("x")
                    sum += 1
                } catch (e: RuntimeException) {
                    sum += 100
                } finally {
                    sum += 10
                }
            }
            1 -> sum += 1000
            else -> sum += 1
        }
    }
    if (sum != 2122) return "got $sum"
    return "OK"
}
"#;
    ok(src, "NestTryWhen");
}

#[test]
fn elvis_in_loop_and_nested_return() {
    let src = r#"
fun firstEven(xs: List<Int>): Int? {
    xs.forEach { if (it % 2 == 0) return it }
    return null
}
fun box(): String {
    val data = listOf<Int?>(null, 1, null, 2)
    var i = 0
    var total = 0
    while ((data.getOrNull(i) ?: 0) < 3 && i < data.size) {
        total += data[i] ?: 0
        i++
    }
    if (total != 3) return "e1 $total"
    if (firstEven(listOf(1, 3, 4, 5)) != 4) return "e2"
    if (firstEven(listOf(1, 3)) != null) return "e3"
    return "OK"
}
"#;
    ok(src, "ElvisLoop");
}

#[test]
fn mutable_list_operations() {
    let src = r#"
fun box(): String {
    val m = mutableListOf(1, 2, 3)
    m.add(4)
    m.add(0, 0)
    if (m != listOf(0, 1, 2, 3, 4)) return "l1"
    m[1] = 20
    if (m[1] != 20) return "l3"
    if (m.indexOf(20) != 1) return "l4"
    val sub = m.subList(1, 3)
    if (sub != listOf(20, 2)) return "l5"
    if (!m.contains(20)) return "l6"
    m.clear()
    if (!m.isEmpty()) return "l7"
    return "OK"
}
"#;
    ok(src, "MutList");
}

#[test]
fn collection_transforms() {
    let src = r#"
fun box(): String {
    val nums = listOf(1, 2, 3, 4, 5, 6)
    val byParity = nums.groupBy { it % 2 }
    if (byParity[0] != listOf(2, 4, 6)) return "g1"
    val (evens, odds) = nums.partition { it % 2 == 0 }
    if (evens != listOf(2, 4, 6) || odds != listOf(1, 3, 5)) return "p1"
    val zipped = listOf("a", "b").zip(listOf(1, 2))
    if (zipped != listOf("a" to 1, "b" to 2)) return "z1"
    val flat = listOf(listOf(1, 2), listOf(3)).flatMap { it }
    if (flat != listOf(1, 2, 3)) return "f1"
    val assoc = listOf("aa", "b", "ccc").associateBy { it.length }
    if (assoc[2] != "aa") return "a1"
    val win = listOf(1, 2, 3, 4).windowed(2)
    if (win.size != 3 || win[0] != listOf(1, 2)) return "w1"
    val ch = listOf(1, 2, 3, 4, 5).chunked(2)
    if (ch.size != 3 || ch[2] != listOf(5)) return "c1"
    return "OK"
}
"#;
    ok(src, "CollTrans");
}

#[test]
fn map_and_set_operations() {
    let src = r#"
fun box(): String {
    val m = mapOf("a" to 1, "b" to 2)
    if (m.getOrDefault("a", 0) != 1) return "g1"
    if (m.getOrDefault("z", 9) != 9) return "g2"
    if (m.getOrElse("z") { 7 } != 7) return "g3"
    val s1 = setOf(1, 2, 3)
    val s2 = setOf(2, 3, 4)
    if (s1.union(s2) != setOf(1, 2, 3, 4)) return "u1"
    if (s1.intersect(s2) != setOf(2, 3)) return "i1"
    if (s1.subtract(s2) != setOf(1)) return "s1"
    return "OK"
}
"#;
    ok(src, "MapSet");
}

#[test]
fn sorting_operations() {
    let src = r#"
data class P(val name: String, val age: Int)
fun box(): String {
    val nums = listOf(3, 1, 2)
    if (nums.sorted() != listOf(1, 2, 3)) return "s1"
    if (nums.sortedDescending() != listOf(3, 2, 1)) return "s2"
    if (nums.reversed() != listOf(2, 1, 3)) return "s3"
    val ppl = listOf(P("b", 30), P("a", 20), P("c", 25))
    val byAge = ppl.sortedBy { it.age }.map { it.name }
    if (byAge != listOf("a", "c", "b")) return "s4"
    val byName = ppl.sortedByDescending { it.name }.map { it.name }
    if (byName != listOf("c", "b", "a")) return "s5"
    return "OK"
}
"#;
    ok(src, "Sorting");
}

#[test]
fn fold_reduce_scan() {
    let src = r#"
fun box(): String {
    val nums = listOf(1, 2, 3, 4)
    if (nums.fold(100) { acc, x -> acc + x } != 110) return "f1"
    if (nums.foldRight("") { x, acc -> acc + x } != "4321") return "f2"
    if (nums.runningFold(0) { acc, x -> acc + x } != listOf(0, 1, 3, 6, 10)) return "sc1"
    if (nums.sum() != 10) return "s1"
    if (nums.joinToString("-") != "1-2-3-4") return "j1"
    return "OK"
}
"#;
    ok(src, "FoldReduce");
}

#[test]
fn first_last_single_predicates() {
    let src = r#"
fun box(): String {
    val nums = listOf(1, 2, 3, 4, 5)
    if (nums.first { it > 2 } != 3) return "f1"
    if (nums.last { it < 4 } != 3) return "l1"
    if (nums.single { it == 3 } != 3) return "s1"
    if (nums.firstOrNull { it > 10 } != null) return "f2"
    if (nums.count { it % 2 == 0 } != 2) return "c1"
    if (nums.find { it > 3 } != 4) return "fi1"
    if (!nums.any { it == 5 }) return "an1"
    if (nums.all { it > 0 } != true) return "al1"
    if (nums.none { it > 10 } != true) return "no1"
    return "OK"
}
"#;
    ok(src, "FirstLast");
}

#[test]
fn arrays_2d_and_copy() {
    let src = r#"
fun box(): String {
    val grid = listOf(listOf(0, 1, 2), listOf(3, 4, 5), listOf(6, 7, 8))
    var sum = 0
    for (row in grid) for (v in row) sum += v
    if (sum != 36) return "g1"
    val a = intArrayOf(1, 2, 3, 4)
    val c = a.copyOf()
    c[0] = 99
    if (a[0] != 1 || c[0] != 99) return "c1"
    val cr = a.copyOfRange(1, 3)
    if (cr.size != 2 || cr[0] != 2) return "c2"
    val filled = IntArray(3)
    filled.fill(7)
    if (filled.sum() != 21) return "f1"
    val toSort = intArrayOf(3, 1, 2)
    toSort.sort()
    if (toSort[0] != 1 || toSort[2] != 3) return "s1"
    return "OK"
}
"#;
    ok(src, "Arrays2D");
}

#[test]
fn primitive_array_aggregates() {
    let src = r#"
fun box(): String {
    val ints = intArrayOf(2, 4, 6, 8)
    if (ints.sum() != 20) return "i1"
    if (ints.average() != 5.0) return "i2"
    if (ints.size != 4) return "i3"
    if (ints.indexOf(6) != 2) return "i4"
    val ds = doubleArrayOf(1.0, 2.0, 3.0)
    if (ds.sum() != 6.0) return "d1"
    if (ds.average() != 2.0) return "d2"
    val longs = longArrayOf(1L, 2L, 3L)
    if (longs.sum() != 6L) return "l1"
    val bytes = byteArrayOf(1, 2, 3)
    if (bytes.sum() != 6) return "b1"
    return "OK"
}
"#;
    ok(src, "PrimArrAgg");
}

#[test]
fn companion_and_object_expression() {
    let src = r#"
class Counter private constructor(val n: Int) {
    companion object {
        fun of(n: Int): Counter = Counter(n)
        const val ZERO: Int = 0
    }
}
interface Greeter { fun greet(): String }
fun box(): String {
    if (Counter.of(5).n != 5) return "c1"
    if (Counter.ZERO != 0) return "c2"
    val g = object : Greeter {
        val who = "world"
        override fun greet(): String = "hi $who"
    }
    if (g.greet() != "hi world") return "o1"
    var captured = 3
    val add = object {
        fun run(): Int = captured + 1
    }
    if (add.run() != 4) return "o2"
    return "OK"
}
"#;
    ok(src, "CompObj");
}

#[test]
fn enum_values_valueof_ordinal() {
    let src = r#"
enum class Dir { NORTH, EAST, SOUTH, WEST }
fun box(): String {
    var acc = 0
    for (d in Dir.values()) acc += d.ordinal
    if (acc != 6) return "e1"
    if (Dir.valueOf("SOUTH").ordinal != 2) return "e2"
    if (Dir.EAST.name != "EAST") return "e3"
    var names = ""
    for (d in Dir.values()) names += d.name[0]
    if (names != "NESW") return "e4"
    var s = ""
    for (d in Dir.values()) if (d.ordinal % 2 == 0) s += d.name[0]
    if (s != "NS") return "e5"
    if (Dir.values().size != 4) return "e6"
    return "OK"
}
"#;
    ok(src, "EnumVals");
}

#[test]
fn sealed_when_and_data_copy() {
    let src = r#"
sealed class Shape
data class Circle(val r: Int) : Shape()
data class Rect(val w: Int, val h: Int) : Shape()
fun area(s: Shape): Int = when (s) {
    is Circle -> 3 * s.r * s.r
    is Rect -> s.w * s.h
}
fun box(): String {
    if (area(Circle(2)) != 12) return "s1"
    if (area(Rect(3, 4)) != 12) return "s2"
    val r = Rect(3, 4)
    val r2 = r.copy(w = 5)
    if (r2.w != 5 || r2.h != 4) return "c1"
    val (w, h) = r
    if (w != 3 || h != 4) return "c2"
    if (r != Rect(3, 4)) return "c3"
    if (r == r2) return "c4"
    return "OK"
}
"#;
    ok(src, "SealedData");
}

#[test]
fn nullable_scope_functions() {
    let src = r#"
class Node(val value: Int, val next: Node?)
fun box(): String {
    val chain = Node(1, Node(2, Node(3, null)))
    val third = chain.next?.next?.value
    if (third != 3) return "n1"
    val missing = chain.next?.next?.next?.value
    if (missing != null) return "n2"
    val r = "hi".let { it.length }
    if (r != 2) return "n3"
    val sb = StringBuilder().also { it.append("x") }
    if (sb.toString() != "x") return "n4"
    val computed = 5.run { this * 2 }
    if (computed != 10) return "n5"
    val nn: Int? = null
    val fromElse = nn?.let { it + 1 } ?: -1
    if (fromElse != -1) return "n6"
    val applied = StringBuilder().apply { append("a"); append("b") }
    if (applied.toString() != "ab") return "n7"
    return "OK"
}
"#;
    ok(src, "NullScope");
}

#[test]
fn tail_recursive_infix_and_operators() {
    let src = r#"
tailrec fun sumTo(n: Int, acc: Int): Int = if (n == 0) acc else sumTo(n - 1, acc + n)
infix fun Int.times2(f: Int): Int = this * f
data class V2(val x: Int, val y: Int) {
    operator fun plus(o: V2) = V2(x + o.x, y + o.y)
    operator fun times(k: Int) = V2(x * k, y * k)
}
fun box(): String {
    if (sumTo(100, 0) != 5050) return "t1"
    if (3 times2 4 != 12) return "i1"
    val v = V2(1, 2) + V2(3, 4)
    if (v != V2(4, 6)) return "o1"
    val w = (V2(1, 1) + V2(1, 1)) * 3
    if (w != V2(6, 6)) return "o2"
    return "OK"
}
"#;
    ok(src, "TailInfix");
}
