//! Consolidated feature `box()` snippets, compiled by krusty and run on a real JVM. To keep the test
//! suite fast, every accepted snippet runs in ONE JVM via a reflective runner (per-snippet
//! `URLClassLoader`), instead of a `javac`+`java` per snippet — the same trick as `box_vendored_e2e`.
//! Each snippet's `box(): String` must return "OK" under `-Xverify:all`.

use std::fs;
use std::process::Command;

mod common;

fn env(k: &str) -> Option<String> {
    std::env::var(k).ok().filter(|v| !v.is_empty())
}

/// `(class-stem, source)` — the file is written as `<stem>.kt`, whose facade class is `<stem>Kt`.
const SNIPPETS: &[(&str, &str)] = &[
    ("Unsigned", r#"
fun box(): String {
    val u1 = 1u; val u2 = 2u
    val u3 = u1 + u2
    if (u3.toInt() != 3) return "f1"
    val a = 42.toUInt()
    if (a.toInt() != 42) return "f2"
    val d = 0u.dec()
    if (d.toLong() != 4294967295L) return "f3"
    val ul = 5uL
    if (ul.toLong() != 5L) return "f4"
    if ((3u - 1u).toInt() != 2) return "f5"
    if (0xFFu.toInt() != 255) return "f6"
    val x = 5u; val y = 3u
    if (x < y) return "f7"
    if (x / y != 1u) return "f8"
    if (x % y != 2u) return "f9"
    if (0u.dec() < x) return "f10"
    if (10uL / 4uL != 2uL) return "f11"
    if (10uL % 4uL != 2uL) return "f12"
    if (10uL < 4uL) return "f13"
    if (0u.dec().toString() != "4294967295") return "f14"
    if ("${0u.dec()}!" != "4294967295!") return "f15"
    if (0uL.dec().toString() != "18446744073709551615") return "f16"
    val any: Any = 5u
    if (any !is UInt) return "f17"
    if (any is Int) return "f18"
    if (any.toString() != "5") return "f19"
    val anyL: Any = 7uL
    if (anyL !is ULong) return "f21"
    var rs = 0u
    for (u in 1u..6u) rs += u
    if (rs != 21u) return "f22"
    var cnt = 0
    for (u in 0u..<4u) cnt++
    if (cnt != 4) return "f23"
    return "OK"
}
"#),
    ("CompanionConst", r#"
const val M = Int.MIN_VALUE
fun box(): String {
    if (Int.MAX_VALUE != 2147483647) return "f1"
    if (Int.MIN_VALUE != -2147483648) return "f2"
    if (Long.MAX_VALUE != 9223372036854775807L) return "f3"
    if (Byte.MAX_VALUE.toInt() != 127) return "f4"
    if (Short.MIN_VALUE.toInt() != -32768) return "f5"
    if (Int.MAX_VALUE * 2L + 1 != 4294967295L) return "f6"
    // non-overflowing loops at the type boundary
    var c1 = 0
    for (i in M downTo M) c1++
    if (c1 != 1) return "f7: $c1"
    var c2 = 0
    for (i in (Int.MAX_VALUE - 2)..Int.MAX_VALUE) c2++
    if (c2 != 3) return "f8: $c2"
    return "OK"
}
"#),
    // A `let`/`also` body containing a branch (`if`/`when`) can't go through the branchless inline
    // splice — it falls back to the per-function desugar, which lowers the branchy body normally.
    ("ScopeFnsBranchy", r#"
fun box(): String {
    val a = 5.let { if (it > 3) "big" else "small" }
    if (a != "big") return "f1:$a"
    val b = 2.let { when { it > 3 -> "x"; else -> "y" } }
    if (b != "y") return "f2:$b"
    var c = ""
    "z".also { c = if (it == "z") "yes" else "no" }
    if (c != "yes") return "f3:$c"
    return "OK"
}
"#),
    ("ScopeFns", r#"
fun box(): String {
    val r = "abc".let { it.length }
    if (r != 3) return "f1: $r"
    var s = 0
    3.let { s += it }
    if (s != 3) return "f2: $s"
    val a = "x".also { s += 5 }
    if (s != 8 || a != "x") return "f3: $s,$a"
    val chain = 5.let { it * 2 }.let { it + 1 }
    if (chain != 11) return "f4: $chain"
    return "OK"
}
"#),
    ("ArrayOfRef", r#"
fun box(): String {
    val a = arrayOf("O", "K")
    if (a[0] + a[1] != "OK") return "f1"
    val b = arrayOf("x", "y", "z")
    var s = ""
    for (e in b) s += e
    if (s != "xyz") return "f2: $s"
    if (b.size != 3) return "f3"
    return "OK"
}
"#),
    // A capturing local function is lifted with its captured locals prepended as parameters: a `val`
    // is passed by value, a `var` it writes is boxed into a shared `Ref` holder (so the mutation is
    // visible to the enclosing scope).
    ("LocalFunCapture", r#"
fun box(): String {
    val base = 100
    fun add(x: Int) = base + x
    if (add(5) != 105) return "f1"
    var acc = 0
    fun bump(x: Int) { acc = acc + x }
    bump(3); bump(4)
    if (acc != 7) return "f2:$acc"
    return "OK"
}
"#),
    // A non-capturing local function is lifted to a private static method on the facade; calls route
    // to it. Recursion and multiple local functions in one body are supported.
    ("LocalFun", r#"
fun box(): String {
    fun dbl(x: Int) = x * 2
    fun fib(n: Int): Int {
        if (n < 2) return n
        return fib(n - 1) + fib(n - 2)
    }
    if (dbl(21) != 42) return "f1"
    if (fib(7) != 13) return "f2"
    return "OK"
}
"#),
    // A mutable local captured and written by a non-inlined lambda (a closure) is boxed into a
    // `kotlin/jvm/internal/Ref$XxxRef` so the closure and the enclosing scope share the cell.
    ("MutableCapture", r#"
fun twice(f: () -> Unit) { f(); f() }
fun call(f: () -> Int): Int = f()
fun box(): String {
    var sum = 0
    twice { sum += 1 }
    twice { sum += 10 }
    if (sum != 22) return "f1:$sum"
    var s = "a"
    twice { s = s + "b" }
    if (s != "abb") return "f2:$s"
    var x = 10
    val r = call { x = x * 2; x }
    if (x != 20 || r != 20) return "f3"
    var c = 0
    twice { c++ }
    twice { c-- ; c-- }
    if (c != -2) return "f4:$c"
    return "OK"
}
"#),
    // Unbound member property reference `A::x` — a synthesized `PropertyReference1Impl` singleton;
    // `.get(receiver)` reads the property via its getter, `.name` is the property name.
    ("PropertyRef", r#"
class A(val x: Int)
fun box(): String {
    val p = A::x
    if (p.get(A(42)) != 42) return "f1"
    if (p.get(A(-1)) != -1) return "f2"
    if (p.name != "x") return "f3"
    return "OK"
}
"#),
    // The literal `-2147483648` is `Int.MIN_VALUE` (an Int), not a Long — usable as an Int `when`
    // branch and in an Int context (the bare `2147483648` overflows Int and is a Long).
    ("IntMinLiteral", r#"
fun cls(x: Int): String = when (x) {
    2147483647 -> "MAX"
    -2147483648 -> "MIN"
    else -> "other"
}
fun box(): String {
    val i: Int = -2147483648
    if (i != -2147483648) return "f1"
    if (cls(-2147483648) != "MIN") return "f2"
    if (cls(2147483647) != "MAX") return "f3"
    return "OK"
}
"#),
    // Method references: bound `obj::m` (receiver captured) and unbound `Type::m` (receiver is the
    // first argument) — each a closure over a synthesized `(receiver, args) -> receiver.m(args)`.
    ("MethodRef", r#"
class C(val p: String) {
    fun get(): String = p
    fun plus(x: String): String = p + x
}
fun box(): String {
    val c = C("OK")
    val bound = c::get
    if (bound() != "OK") return "f1"
    val unbound = C::plus
    if (unbound(C("A"), "B") != "AB") return "f2"
    return "OK"
}
"#),
    // A `Unit`-returning function reference `::add` wraps the call and returns the Unit singleton
    // (a direct method handle would adapt `void` to `null`, breaking a `FunctionN` consumer).
    ("UnitFunRef", r#"
val sb = StringBuilder()
fun add(s: String) { sb.append(s) }
fun apply2(f: (String) -> Unit) { f("O"); f("K") }
fun box(): String {
    apply2(::add)
    return sb.toString()
}
"#),
    // Constructor reference `::A` — a closure wrapping `new A(args)`, usable as a `FunctionN` value.
    ("CtorRef", r#"
class A(val result: String)
class P(val x: Int, val y: Int)
fun box(): String {
    val f = ::A
    if (f("OK").result != "OK") return "f1"
    val g = ::P
    val p = g(3, 4)
    if (p.x != 3 || p.y != 4) return "f2"
    return "OK"
}
"#),
    // Enum entries with a body: each bodied entry is a synthesized subclass (`Op$ADD extends Op`)
    // overriding an abstract member; the override can read an enum constructor `val`.
    ("EnumEntryBody", r#"
enum class Op(val sym: String) {
    ADD("+") { override fun apply(a: Int, b: Int) = a + b },
    MUL("*") { override fun apply(a: Int, b: Int) = a * b };
    abstract fun apply(a: Int, b: Int): Int
}
fun box(): String {
    if (Op.ADD.apply(2, 3) != 5) return "f1"
    if (Op.MUL.apply(2, 3) != 6) return "f2"
    if (Op.ADD.sym != "+") return "f3"
    if (Op.MUL.sym != "*") return "f4"
    return "OK"
}
"#),
    // The overridable members compareTo/equals/hashCode have Kotlin-contract return types (Int/Boolean/
    // Int), used when the body can't be inferred locally (`compareTo(o) = v - o.v` references `o`).
    ("CompareToContract", r#"
class N(val v: Int) {
    operator fun compareTo(o: N) = v - o.v
}
fun box(): String {
    if (!(N(3) < N(5))) return "f1"
    if (!(N(7) > N(2))) return "f2"
    if (!(N(4) <= N(4))) return "f3"
    return "OK"
}
"#),
    // Class member operators: `a + b` → `a.plus(b)` (and minus/times/div/rem); `a < b` →
    // `a.compareTo(b) < 0`.
    ("ClassOperators", r#"
class V(val x: Int) {
    operator fun plus(o: V) = V(x + o.x)
    operator fun minus(o: V) = V(x - o.x)
    operator fun times(o: V) = V(x * o.x)
    operator fun compareTo(o: V): Int = x - o.x
}
fun box(): String {
    if ((V(1) + V(2)).x != 3) return "f1"
    if ((V(7) - V(3)).x != 4) return "f2"
    if ((V(4) * V(5)).x != 20) return "f3"
    if (!(V(2) < V(5))) return "f4"
    if (!(V(9) >= V(9))) return "f5"
    if ((V(1) + V(2) + V(3)).x != 6) return "f6"
    return "OK"
}
"#),
    // An expression-bodied extension function with no explicit return type infers it from the body,
    // with `this` bound to the receiver (`fun Int.double() = this * 2` → return Int).
    ("ExtThisInfer", r#"
fun Int.double() = this * 2
fun String.shout() = this + "!"
fun Int.isPos() = this > 0
fun box(): String {
    if (5.double() != 10) return "f1"
    if ("hi".shout() != "hi!") return "f2"
    if (!4.isPos()) return "f3"
    return "OK"
}
"#),
    // A deferred `val` (declared with a type, no initializer) is assigned exactly once in an `init`
    // block — a real backing field initialized in the constructor body.
    ("DeferredValInit", r#"
class C(x: Int) {
    val a: Int
    val b: Int
    init {
        a = x
        b = x + 1
    }
}
fun box(): String {
    val o = C(5)
    if (o.a != 5) return "f1"
    if (o.b != 6) return "f2"
    return "OK"
}
"#),
    // A non-`val`/`var` primary-constructor parameter is an argument only (no field), available in the
    // constructor body for property initializers and `init` blocks — including interleaved with `val`s.
    ("NonPropertyCtorParam", r#"
class A(x: Int) { val y = x * 2 }
class B(val a: Int, b: Int, val c: Int) { val sum = a + b + c }
class C(x: Int) { var z = 0; init { z = x + 10 } }
class D(name: String) { val greeting = "Hi " + name }
fun box(): String {
    if (A(3).y != 6) return "f1"
    val b = B(1, 2, 3)
    if (b.a != 1 || b.c != 3 || b.sum != 6) return "f2"
    if (C(5).z != 15) return "f3"
    if (D("Bob").greeting != "Hi Bob") return "f4"
    return "OK"
}
"#),
    // A body property's type is inferred from its initializer with the preceding properties (and
    // val/var ctor params) in scope: `val b = a + 1` sees the earlier `a`.
    ("SequentialPropInfer", r#"
class C(val x: Int) {
    val a = 10
    val b = a + 1
    val c = b * x
}
fun box(): String {
    val o = C(2)
    if (o.a != 10) return "f1"
    if (o.b != 11) return "f2"
    if (o.c != 22) return "f3:${o.c}"
    return "OK"
}
"#),
    // A private @InlineOnly String extension (`uppercase`/`lowercase` → `toUpperCase(Locale.ROOT)`) is
    // inlined from its real stdlib bytecode (it has no callable body and no JDK member equivalent).
    ("StringInlineExt", r#"
fun box(): String {
    if ("ab".uppercase() != "AB") return "f1"
    if ("AB".lowercase() != "ab") return "f2"
    if (" Ab ".trim().uppercase() != "AB") return "f3"
    return "OK"
}
"#),
    // String members resolve to their java.lang.String JVM methods (a member wins over a same-named
    // private @InlineOnly extension like StringsKt.isEmpty).
    ("StringMembers", r#"
fun box(): String {
    if ("abc".isEmpty()) return "f1"
    if (!"".isEmpty()) return "f2"
    if (!"abc".startsWith("ab")) return "f3"
    if ("abc".indexOf("b") != 1) return "f4"
    return "OK"
}
"#),
    // A data class ALWAYS generates equals/hashCode/toString over an OPEN base member (KT-6206), but
    // INHERITS a `final` base member (can't override it).
    ("DataClassOverBase", r#"
abstract class Open { override fun toString() = "base" }
data class D1(val f: String) : Open()
abstract class Final { final override fun toString() = "kept" }
data class D2(val f: String) : Final()
fun box(): String {
    if (D1("x").toString() != "D1(f=x)") return "f1:${D1("x")}"
    if (D2("x").toString() != "kept") return "f2:${D2("x")}"
    return "OK"
}
"#),
    // Kotlin's built-in collection mapped members: `Map.keys`/`entries` resolve to the JVM
    // `keySet()`/`entrySet()` (Map.values/size keep their JVM name and already worked).
    ("MapMappedMembers", r#"
fun box(): String {
    val m = mapOf(1 to "a", 2 to "b", 3 to "c")
    if (m.keys.size != 3) return "f1"
    if (m.entries.size != 3) return "f2"
    if (m.values.size != 3) return "f3"
    if (m.size != 3) return "f4"
    if (!m.keys.contains(2)) return "f5"
    return "OK"
}
"#),
    // Legal nested-scope variable shadowing: an inner block's `val x` shadows an outer `x` (each gets
    // its own slot; the outer is restored at block exit). Same-scope redeclaration is still an error.
    ("Shadowing", r#"
fun box(): String {
    val x = 1
    var sum = 0
    for (i in 0..2) {
        val x = i * 10
        sum += x
    }
    if (sum != 30) return "f1:$sum"
    if (x != 1) return "f2:$x"
    if (true) {
        val x = "abc"
        if (x.length != 3) return "f3"
    }
    return if (x == 1) "OK" else "f4:$x"
}
"#),
    // Nested try/catch (without a finally in the nest) compiles and runs; only a nested-try combined
    // with a finally is rejected (skip), never miscompiled.
    ("NestedTry", r#"
fun box(): String {
    var r = ""
    try {
        try {
            r += "a"
            throw RuntimeException("x")
        } catch (e: RuntimeException) {
            r += "b"
        }
        r += "c"
    } catch (e: Exception) {
        r += "z"
    }
    return if (r == "abc") "OK" else "F:$r"
}
"#),
    // A bare-value lambda types its parameters from its own annotations (`{ x: Int -> … }`), even with
    // no expected function type — so the body and a direct call both check correctly.
    ("LambdaParamType", r#"
fun box(): String {
    val dbl = { x: Int -> x * 2 }
    if (dbl(3) != 6) return "f1"
    val add = { a: Int, b: Int -> a + b }
    if (add(2, 5) != 7) return "f2"
    val len = { s: String -> s.length }
    if (len("abcd") != 4) return "f3"
    return "OK"
}
"#),
    // Labeled loops: `break@label`/`continue@label` target the named enclosing loop.
    ("LabeledLoops", r#"
fun box(): String {
    var s = 0
    outer@ for (i in 0 until 3) {
        for (j in 0 until 3) {
            if (j == 2) continue@outer
            if (i == 2) break@outer
            s += 1
        }
    }
    if (s != 4) return "f1:$s"
    var t = 0
    loop@ for (x in listOf(1, 2, 3, 4)) {
        if (x == 3) break@loop
        t += x
    }
    if (t != 3) return "f2:$t"
    return "OK"
}
"#),
    // The array creators are intrinsics keyed on the resolved stdlib symbol: a user function of the
    // same name must shadow the intrinsic (as in kotlinc), not be silently lowered to an array.
    ("ArrayOfUserShadow", r#"
fun arrayOf(a: String): String = "user:$a"
fun box(): String {
    val r = arrayOf("x")
    return if (r == "user:x") "OK" else "F:$r"
}
"#),
    ("RepeatInline", r#"
fun box(): String {
    var s = 0
    repeat(4) { s += it }
    if (s != 6) return "f1: $s"
    var c = 0
    repeat(5) { c++ }
    if (c != 5) return "f2: $c"
    val sb = StringBuilder()
    repeat(3) { sb.append("x") }
    if (sb.toString() != "xxx") return "f3"
    return "OK"
}
"#),
    ("ForeachInline", r#"
fun box(): String {
    var s = 0
    listOf(1, 2, 3, 4).forEach { s += it }
    if (s != 10) return "f1: $s"
    var p = 1
    setOf(2, 3, 5).forEach { p *= it }
    if (p != 30) return "f2: $p"
    val sb = StringBuilder()
    listOf("a", "b", "c").forEach { sb.append(it) }
    if (sb.toString() != "abc") return "f3: $sb"
    var w = 0
    listOf(10, 20, 30).forEachIndexed { i, x -> w += (i + 1) * x }
    if (w != 140) return "f4: $w"
    // array + String forEach (inlined index loop) with mutable capture
    var asum = 0
    intArrayOf(1, 2, 3, 4).forEach { asum += it }
    if (asum != 10) return "f5: $asum"
    var csum = 0
    "abc".forEach { csum += it.code }
    if (csum != 'a'.code + 'b'.code + 'c'.code) return "f6: $csum"
    return "OK"
}
"#),
    ("MapIndexed", r#"
fun box(): String {
    val r = listOf(10, 20, 30).mapIndexed { i, x -> i * x + 1 }
    if (r != listOf(1, 21, 61)) return "f1: $r"
    val r2 = listOf("a", "bb", "ccc").mapIndexed { i, s -> i + s.length }
    if (r2 != listOf(1, 3, 5)) return "f2: $r2"
    return "OK"
}
"#),
    ("IncDec", r#"
fun ident(n: Int): Int = n
fun box(): String {
    var i = 5
    val a = i++
    if (a != 5 || i != 6) return "f1"
    val b = ++i
    if (b != 7 || i != 7) return "f2"
    var j = 3
    if (j-- != 3 || j != 2) return "f3"
    if (--j != 1 || j != 1) return "f4"
    var k = 0
    if ((k++) + (k++) != 1 || k != 2) return "f5"
    var m = 3
    if (ident(m--) != 3 || m != 2) return "f6"
    var t = 0
    if ("${t++}x" != "0x" || t != 1) return "f7"
    var w = 0
    when (w++) { 0 -> {} else -> {} }
    if (w != 1) return "f8"
    var n = 0; n++; ++n
    if (n != 2) return "f9"
    var by1: Byte = 127; by1++
    if (by1.toInt() != -128) return "f10"
    var by2: Byte = 127
    val ob = by2++
    if (ob.toInt() != 127 || by2.toInt() != -128) return "f11"
    var sh: Short = 32767; sh++
    if (sh.toInt() != -32768) return "f12"
    var ch = 'a'
    val oc = ch++
    if (oc != 'a' || ch != 'b') return "f13"
    return "OK"
}
"#),
    ("UserInline", r#"
inline fun twice(block: () -> Unit) { block(); block() }
inline fun applyN(n: Int, block: (Int) -> Unit) { var i = 0; while (i < n) { block(i); i++ } }
inline fun pick(c: Boolean, a: () -> Int, b: () -> Int): Int = if (c) a() else b()
fun box(): String {
    var s = 0
    twice { s += 3 }
    if (s != 6) return "f1: $s"
    var acc = 0
    applyN(4) { acc += it }
    if (acc != 6) return "f2: $acc"
    val r = pick(true, { 10 }, { 20 })
    if (r != 10) return "f3: $r"
    // nested inline calls + mutable capture across both
    var t = 0
    twice { applyN(3) { t += it } }
    if (t != 6) return "f4: $t"
    return "OK"
}
"#),
    ("RangeValue", r#"
fun box(): String {
    val r = 0..3
    if (r.first != 0) return "f1"
    if (r.last != 3) return "f2"
    var s = 0
    for (x in r) s += x
    if (s != 6) return "f3"
    if ((1..<4).last != 3) return "f4"
    val lr = 10L..12L
    if (lr.last != 12L) return "f5a"
    var lo = 0L
    for (y in lr) lo += y
    if (lo != 33L) return "f5"
    var t = 0
    for (z in 5..7) t += z
    if (t != 18) return "f6"
    var cs = 0
    for (c in 'a'..'e') cs += c.code
    if (cs != 'a'.code + 'b'.code + 'c'.code + 'd'.code + 'e'.code) return "f7"
    var lt = 0L
    for (y in 1L..4L) lt += y
    if (lt != 10L) return "f8"
    return "OK"
}
"#),
];

#[test]
fn feature_snippets_run() {
    let Some(java_home) = env("KRUSTY_REF_JAVA_HOME").or_else(|| env("JAVA_HOME")) else {
        eprintln!("skipping feature_box_e2e: set JAVA_HOME");
        return;
    };
    let java = format!("{java_home}/bin/java");
    let javac = format!("{java_home}/bin/javac");
    if !std::path::Path::new(&javac).exists() {
        return;
    }
    let Some(stdlib) = common::stdlib_jar() else {
        eprintln!("skipping feature_box_e2e: no kotlin-stdlib jar found");
        return;
    };
    let stdlib = stdlib.to_str().unwrap().to_string();
    let jdk_modules = format!("{java_home}/lib/modules");
    let compile_cp = if std::path::Path::new(&jdk_modules).exists() {
        format!("{stdlib}:{jdk_modules}")
    } else {
        stdlib.clone()
    };
    let krusty = env!("CARGO_BIN_EXE_krusty");
    let work = std::env::temp_dir().join(format!("krusty_feat_{}", std::process::id()));
    let _ = fs::remove_dir_all(&work);
    fs::create_dir_all(&work).unwrap();

    // Reflective runner compiled once.
    let runner = work.join("runner");
    fs::create_dir_all(&runner).unwrap();
    let runner_src = r#"import java.io.File; import java.net.URL; import java.net.URLClassLoader;
public class BoxRun {
  public static void main(String[] args) throws Exception {
    for (int i = 0; i + 1 < args.length; i += 2) {
      String result;
      try {
        URLClassLoader cl = new URLClassLoader(new URL[]{ new File(args[i]).toURI().toURL() }, BoxRun.class.getClassLoader());
        Object r = Class.forName(args[i+1], true, cl).getMethod("box").invoke(null);
        result = String.valueOf(r);
      } catch (Throwable t) { result = "EXC:" + t; }
      System.out.println(args[i+1] + "\t" + result);
    }
  }
}
"#;
    fs::write(runner.join("BoxRun.java"), runner_src).unwrap();
    let jc = Command::new(&javac).args(["-d", runner.to_str().unwrap()]).arg(runner.join("BoxRun.java")).output().unwrap();
    assert!(jc.status.success(), "javac(BoxRun): {}", String::from_utf8_lossy(&jc.stderr));

    // Compile every snippet with krusty into its own dir.
    let mut cases: Vec<(String, String)> = Vec::new(); // (dir, boxClass)
    for (i, (stem, src)) in SNIPPETS.iter().enumerate() {
        let dir = work.join(format!("s{i}"));
        fs::create_dir_all(&dir).unwrap();
        let kt = dir.join(format!("{stem}.kt"));
        fs::write(&kt, src).unwrap();
        let out = Command::new(krusty).args(["-cp", &compile_cp, "-d", dir.to_str().unwrap()]).arg(&kt).output().unwrap();
        assert!(out.status.success(), "krusty {stem}: {}", String::from_utf8_lossy(&out.stderr));
        cases.push((dir.to_str().unwrap().to_string(), format!("{stem}Kt")));
    }

    // Run all snippets in one JVM.
    let mut cp = runner.to_str().unwrap().to_string();
    cp.push(':');
    cp.push_str(&stdlib);
    let mut args: Vec<String> = vec!["-Xverify:all".into(), "-cp".into(), cp, "BoxRun".into()];
    for (dir, class) in &cases {
        args.push(dir.clone());
        args.push(class.clone());
    }
    let run = Command::new(&java).args(&args).output().unwrap();
    assert!(run.status.success(), "BoxRun: {}", String::from_utf8_lossy(&run.stderr));
    let stdout = String::from_utf8_lossy(&run.stdout);
    let results: std::collections::HashMap<&str, &str> = stdout.lines().filter_map(|l| l.split_once('\t')).collect();
    for (_, class) in &cases {
        let got = results.get(class.as_str()).copied().unwrap_or("<missing>");
        assert!(got == "OK", "{class}.box() returned {got:?} (all: {stdout})");
    }
    let _ = fs::remove_dir_all(&work);
}
