//! End-to-end "box" coverage for everyday Kotlin surface: lambdas / higher-order functions and
//! closures, collections and their ops, null-safety, operator overloading, extension functions and
//! properties, and string templates. Each test compiles a `box()` snippet with krusty, runs it on
//! the JVM, and asserts it returns "OK" only when the internal value checks hold.

use super::common;

/// Compile `src`, run `box()` on the JVM, and assert it returns "OK". Skips (does not fail) when the
/// toolchain is unavailable so the suite stays green off a provisioned machine.
fn run_ok(src: &str, stem: &str) {
    common::assert_box_ok_with_stdlib(src, stem);
}

#[test]
fn higher_order_and_it() {
    let src = "fun apply2(x: Int, f: (Int) -> Int): Int = f(f(x))\n\
fun box(): String {\n\
if (apply2(3) { it + 1 } != 5) return \"f1\"\n\
if (apply2(10) { it * 2 } != 40) return \"f2\"\n\
val g: (Int) -> Int = { n -> n * n }\n\
if (g(6) != 36) return \"f3\"\n\
return \"OK\"\n\
}\n";
    run_ok(src, "HoIt");
}

#[test]
fn closure_captures_local() {
    let src = "fun makeAdder(base: Int): (Int) -> Int { return { x -> x + base } }\n\
fun box(): String {\n\
val add10 = makeAdder(10)\n\
if (add10(5) != 15) return \"f1\"\n\
var total = 0\n\
val acc = { n: Int -> total += n }\n\
acc(3); acc(4)\n\
if (total != 7) return \"f2\"\n\
return \"OK\"\n\
}\n";
    run_ok(src, "Closure");
}

#[test]
fn list_ops() {
    let src = "fun box(): String {\n\
val xs = listOf(1, 2, 3, 4)\n\
if (xs.size != 4) return \"f1\"\n\
if (xs[2] != 3) return \"f2\"\n\
var sum = 0\n\
for (x in xs) sum += x\n\
if (sum != 10) return \"f3\"\n\
if (xs.sum() != 10) return \"f4\"\n\
return \"OK\"\n\
}\n";
    run_ok(src, "ListOps");
}

#[test]
fn list_filter_map() {
    let src = "fun box(): String {\n\
val xs = listOf(1, 2, 3, 4, 5, 6)\n\
val evens = xs.filter { it % 2 == 0 }\n\
if (evens.size != 3) return \"f1\"\n\
val doubled = xs.map { it * 2 }\n\
if (doubled[0] != 2) return \"f2\"\n\
if (doubled.sum() != 42) return \"f3\"\n\
return \"OK\"\n\
}\n";
    run_ok(src, "FilterMap");
}

#[test]
fn map_and_set() {
    let src = "fun box(): String {\n\
val m = mapOf(\"a\" to 1, \"b\" to 2)\n\
if (m.size != 2) return \"f1\"\n\
if (m[\"a\"] != 1) return \"f2\"\n\
val s = setOf(1, 2, 2, 3)\n\
if (s.size != 3) return \"f3\"\n\
if (!s.contains(2)) return \"f4\"\n\
return \"OK\"\n\
}\n";
    run_ok(src, "MapSet");
}

#[test]
fn null_safety() {
    let src = "fun pick(b: Boolean): String? = if (b) \"hi\" else null\n\
fun box(): String {\n\
val a: String? = pick(true)\n\
if (a?.length != 2) return \"f1\"\n\
val b: String? = pick(false)\n\
if ((b?.length ?: -1) != -1) return \"f2\"\n\
val c = b ?: \"fallback\"\n\
if (c != \"fallback\") return \"f3\"\n\
if (a!!.length != 2) return \"f4\"\n\
return \"OK\"\n\
}\n";
    run_ok(src, "NullSafe");
}

#[test]
fn null_smart_cast() {
    let src = "fun lenOf(s: String?): Int {\n\
if (s == null) return -1\n\
return s.length\n\
}\n\
fun box(): String {\n\
if (lenOf(null) != -1) return \"f1\"\n\
if (lenOf(\"abcd\") != 4) return \"f2\"\n\
return \"OK\"\n\
}\n";
    run_ok(src, "SmartCast");
}

#[test]
fn operator_plus_and_get() {
    let src = "class Vec2(val x: Int, val y: Int) {\n\
operator fun plus(o: Vec2): Vec2 = Vec2(x + o.x, y + o.y)\n\
operator fun get(i: Int): Int = if (i == 0) x else y\n\
}\n\
fun box(): String {\n\
val a = Vec2(1, 2)\n\
val b = Vec2(3, 4)\n\
val c = a + b\n\
if (c.x != 4) return \"f1\"\n\
if (c.y != 6) return \"f2\"\n\
if (c[0] != 4) return \"f3\"\n\
if (c[1] != 6) return \"f4\"\n\
return \"OK\"\n\
}\n";
    run_ok(src, "OpGet");
}

#[test]
fn operator_compare_to() {
    let src = "class Money(val cents: Int) : Comparable<Money> {\n\
override fun compareTo(other: Money): Int = cents - other.cents\n\
}\n\
fun box(): String {\n\
val a = Money(100)\n\
val b = Money(250)\n\
if (!(a < b)) return \"f1\"\n\
if (a > b) return \"f2\"\n\
if (!(b >= a)) return \"f3\"\n\
return \"OK\"\n\
}\n";
    run_ok(src, "OpCmp");
}

#[test]
fn extension_fun_and_property() {
    let src = "class Box(val n: Int)\n\
fun Box.doubled(): Int = n * 2\n\
val Box.triple: Int get() = n * 3\n\
fun Int.timesTen(): Int = this * 10\n\
fun box(): String {\n\
val b = Box(5)\n\
if (b.doubled() != 10) return \"f1\"\n\
if (b.triple != 15) return \"f2\"\n\
if (7.timesTen() != 70) return \"f3\"\n\
return \"OK\"\n\
}\n";
    run_ok(src, "ExtFun");
}

#[test]
fn extension_on_stdlib_type() {
    let src = "fun String.shout(): String = this + \"!\"\n\
val String.firstOrDash: Char get() = if (this.isEmpty()) '-' else this[0]\n\
fun box(): String {\n\
if (\"hi\".shout() != \"hi!\") return \"f1\"\n\
if (\"abc\".firstOrDash != 'a') return \"f2\"\n\
if (\"\".firstOrDash != '-') return \"f3\"\n\
return \"OK\"\n\
}\n";
    run_ok(src, "ExtStd");
}

#[test]
fn string_templates() {
    let src = "fun box(): String {\n\
val x = 7\n\
val name = \"world\"\n\
val s1 = \"$x items\"\n\
if (s1 != \"7 items\") return \"f1\"\n\
val s2 = \"hello $name\"\n\
if (s2 != \"hello world\") return \"f2\"\n\
val s3 = \"sum=${x + 3}\"\n\
if (s3 != \"sum=10\") return \"f3\"\n\
val s4 = \"len=${name.length}\"\n\
if (s4 != \"len=5\") return \"f4\"\n\
return \"OK\"\n\
}\n";
    run_ok(src, "StrTpl");
}
