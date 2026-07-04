//! End-to-end "box" coverage for core language features: data classes (componentN /
//! destructuring / copy / equals / hashCode / toString), sealed hierarchies with exhaustive
//! `when` + smart casts, enum classes (constructor param, method, values/valueOf/ordinal/
//! name/entries), generics (generic fun + generic class), and `when` with ranges / `in`.
//! Each snippet is self-contained (only kotlin-stdlib) and returns "OK" iff every assert holds.

mod common;

/// Compile `src` (containing `fun box(): String`) with krusty, run on the JVM, expect "OK".
fn run_ok(src: &str, stem: &str) {
    common::assert_box_ok_with_stdlib(src, stem);
}

#[test]
fn data_class_destructuring_and_component() {
    let src = "data class Point(val x: Int, val y: Int)\n\
fun box(): String {\n\
val p = Point(3, 4)\n\
val (a, b) = p\n\
if (a != 3) return \"f1\"\n\
if (b != 4) return \"f2\"\n\
if (p.component1() != 3) return \"f3\"\n\
if (p.component2() != 4) return \"f4\"\n\
return \"OK\"\n\
}\n";
    run_ok(src, "DataComp");
}

#[test]
fn data_class_copy() {
    let src = "data class Person(val name: String, val age: Int)\n\
fun box(): String {\n\
val p = Person(\"Ann\", 30)\n\
val q = p.copy(age = 31)\n\
if (q.name != \"Ann\") return \"f1\"\n\
if (q.age != 31) return \"f2\"\n\
if (p.age != 30) return \"f3\"\n\
val r = p.copy(name = \"Bob\")\n\
if (r.name != \"Bob\") return \"f4\"\n\
if (r.age != 30) return \"f5\"\n\
return \"OK\"\n\
}\n";
    run_ok(src, "DataCopy2");
}

#[test]
fn data_class_equals_hashcode_tostring() {
    let src = "data class Pair2(val a: Int, val b: String)\n\
fun box(): String {\n\
val x = Pair2(1, \"z\")\n\
val y = Pair2(1, \"z\")\n\
val w = Pair2(2, \"z\")\n\
if (x != y) return \"f1\"\n\
if (x == w) return \"f2\"\n\
if (x.hashCode() != y.hashCode()) return \"f3\"\n\
if (x.toString() != \"Pair2(a=1, b=z)\") return \"f4:\" + x.toString()\n\
return \"OK\"\n\
}\n";
    run_ok(src, "DataEq");
}

#[test]
fn sealed_hierarchy_exhaustive_when() {
    let src = "sealed class Shape\n\
class Circle(val r: Int) : Shape()\n\
class Rect(val w: Int, val h: Int) : Shape()\n\
fun area(s: Shape): Int = when (s) {\n\
is Circle -> s.r * s.r * 3\n\
is Rect -> s.w * s.h\n\
}\n\
fun box(): String {\n\
if (area(Circle(2)) != 12) return \"f1\"\n\
if (area(Rect(3, 4)) != 12) return \"f2\"\n\
return \"OK\"\n\
}\n";
    run_ok(src, "SealedWhen");
}

#[test]
fn sealed_smart_cast_object_arm() {
    let src = "sealed class Expr\n\
class Lit(val v: Int) : Expr()\n\
class Add(val l: Expr, val r: Expr) : Expr()\n\
object Zero : Expr()\n\
fun eval(e: Expr): Int = when (e) {\n\
is Lit -> e.v\n\
is Add -> eval(e.l) + eval(e.r)\n\
Zero -> 0\n\
}\n\
fun box(): String {\n\
val e = Add(Lit(5), Add(Lit(2), Zero))\n\
if (eval(e) != 7) return \"f1\"\n\
if (eval(Zero) != 0) return \"f2\"\n\
return \"OK\"\n\
}\n";
    run_ok(src, "SealedSmart");
}

#[test]
fn enum_constructor_param_and_method() {
    let src = "enum class Planet(val mass: Int) {\n\
EARTH(5),\n\
MARS(1),\n\
JUPITER(300);\n\
fun heavy(): Boolean = mass > 100\n\
}\n\
fun box(): String {\n\
if (Planet.EARTH.mass != 5) return \"f1\"\n\
if (!Planet.JUPITER.heavy()) return \"f2\"\n\
if (Planet.MARS.heavy()) return \"f3\"\n\
return \"OK\"\n\
}\n";
    run_ok(src, "EnumCtor");
}

#[test]
fn enum_values_valueof_ordinal_name() {
    let src = "enum class Color { RED, GREEN, BLUE }\n\
fun box(): String {\n\
if (Color.RED.ordinal != 0) return \"f1\"\n\
if (Color.BLUE.ordinal != 2) return \"f2\"\n\
if (Color.GREEN.name != \"GREEN\") return \"f3\"\n\
if (Color.valueOf(\"BLUE\") != Color.BLUE) return \"f4\"\n\
val vs = Color.values()\n\
if (vs.size != 3) return \"f5\"\n\
if (vs[1] != Color.GREEN) return \"f6\"\n\
return \"OK\"\n\
}\n";
    run_ok(src, "EnumValues");
}

#[test]
fn enum_entries() {
    let src = "enum class Dir { N, E, S, W }\n\
fun box(): String {\n\
val e = Dir.entries\n\
if (e.size != 4) return \"f1\"\n\
if (e[0] != Dir.N) return \"f2\"\n\
if (e[3] != Dir.W) return \"f3\"\n\
var count = 0\n\
for (d in Dir.entries) count += d.ordinal\n\
if (count != 6) return \"f4\"\n\
return \"OK\"\n\
}\n";
    run_ok(src, "EnumEntries2");
}

#[test]
fn generic_function() {
    let src = "fun <T> identity(x: T): T = x\n\
fun <T> firstOf(a: T, b: T): T = a\n\
fun box(): String {\n\
if (identity(7) != 7) return \"f1\"\n\
if (identity(\"hi\") != \"hi\") return \"f2\"\n\
if (firstOf(\"a\", \"b\") != \"a\") return \"f3\"\n\
if (firstOf(10, 20) != 10) return \"f4\"\n\
return \"OK\"\n\
}\n";
    run_ok(src, "GenFn2");
}

#[test]
fn generic_class_holding_value() {
    let src = "class Box<T>(val value: T) {\n\
fun get(): T = value\n\
}\n\
fun box(): String {\n\
val bi = Box(42)\n\
val bs = Box(\"text\")\n\
if (bi.get() != 42) return \"f1\"\n\
if (bs.get() != \"text\") return \"f2\"\n\
if (bi.value != 42) return \"f3\"\n\
return \"OK\"\n\
}\n";
    run_ok(src, "GenBox2");
}

#[test]
fn when_with_ranges_and_in() {
    let src = "fun grade(n: Int): String = when (n) {\n\
in 90..100 -> \"A\"\n\
in 80..89 -> \"B\"\n\
in 70..79 -> \"C\"\n\
else -> \"F\"\n\
}\n\
fun box(): String {\n\
if (grade(95) != \"A\") return \"f1\"\n\
if (grade(85) != \"B\") return \"f2\"\n\
if (grade(72) != \"C\") return \"f3\"\n\
if (grade(50) != \"F\") return \"f4\"\n\
val x = 5\n\
val r = when {\n\
x in 1..3 -> \"low\"\n\
x in 4..6 -> \"mid\"\n\
else -> \"hi\"\n\
}\n\
if (r != \"mid\") return \"f5\"\n\
return \"OK\"\n\
}\n";
    run_ok(src, "WhenRange2");
}

#[test]
fn when_over_enum_subject() {
    let src = "enum class Light { RED, YELLOW, GREEN }\n\
fun act(l: Light): String = when (l) {\n\
Light.RED -> \"stop\"\n\
Light.YELLOW -> \"slow\"\n\
Light.GREEN -> \"go\"\n\
}\n\
fun box(): String {\n\
if (act(Light.RED) != \"stop\") return \"f1\"\n\
if (act(Light.GREEN) != \"go\") return \"f2\"\n\
if (act(Light.YELLOW) != \"slow\") return \"f3\"\n\
return \"OK\"\n\
}\n";
    run_ok(src, "WhenEnum2");
}
