//! End-to-end "box" coverage for sealed hierarchies, enums, data classes, object/anonymous
//! expressions, companion objects, and typealiases. Each test compiles a self-contained
//! `fun box(): String` returning "OK" and runs it on the JVM under the persistent box runner.

mod common;

/// Compile `src` (with a `box()` returning "OK") under `stem` against the stdlib + JDK modules and
/// assert it prints "OK". Skips (returns) when the toolchain isn't provisioned.
fn run_ok(src: &str, stem: &str) {
    common::assert_box_ok_with_stdlib(src, stem);
}

#[test]
fn sealed_interface_exhaustive_when() {
    let src = "sealed interface Shape\n\
class Circle(val r: Int) : Shape\n\
class Square(val s: Int) : Shape\n\
fun area(sh: Shape): Int = when (sh) {\n\
    is Circle -> sh.r * sh.r * 3\n\
    is Square -> sh.s * sh.s\n\
}\n\
fun box(): String {\n\
    if (area(Circle(2)) != 12) return \"f1\"\n\
    if (area(Square(3)) != 9) return \"f2\"\n\
    return \"OK\"\n\
}\n";
    run_ok(src, "SealedIface");
}

#[test]
fn sealed_class_data_subtypes_smartcast() {
    let src = "sealed class Expr\n\
data class Num(val v: Int) : Expr()\n\
data class Add(val l: Expr, val r: Expr) : Expr()\n\
fun eval(e: Expr): Int = when (e) {\n\
    is Num -> e.v\n\
    is Add -> eval(e.l) + eval(e.r)\n\
}\n\
fun box(): String {\n\
    val e = Add(Num(3), Add(Num(4), Num(5)))\n\
    if (eval(e) != 12) return \"f1\"\n\
    return \"OK\"\n\
}\n";
    run_ok(src, "SealedData");
}

#[test]
fn enum_implementing_interface() {
    let src = "interface Named { fun label(): String }\n\
enum class Color(val rgb: Int) : Named {\n\
    RED(0xff0000) { override fun label() = \"r\" },\n\
    GREEN(0x00ff00) { override fun label() = \"g\" };\n\
}\n\
fun box(): String {\n\
    if (Color.RED.label() != \"r\") return \"f1\"\n\
    if (Color.GREEN.rgb != 0x00ff00) return \"f2\"\n\
    return \"OK\"\n\
}\n";
    run_ok(src, "EnumIface");
}

#[test]
fn enum_abstract_method_per_entry() {
    let src = "enum class Op {\n\
    PLUS { override fun apply(a: Int, b: Int) = a + b },\n\
    TIMES { override fun apply(a: Int, b: Int) = a * b };\n\
    abstract fun apply(a: Int, b: Int): Int\n\
}\n\
fun box(): String {\n\
    if (Op.PLUS.apply(2, 3) != 5) return \"f1\"\n\
    if (Op.TIMES.apply(2, 3) != 6) return \"f2\"\n\
    return \"OK\"\n\
}\n";
    run_ok(src, "EnumAbstract");
}

#[test]
fn enum_exhaustive_when() {
    let src = "enum class Dir { N, E, S, W }\n\
fun opposite(d: Dir): Dir = when (d) {\n\
    Dir.N -> Dir.S\n\
    Dir.S -> Dir.N\n\
    Dir.E -> Dir.W\n\
    Dir.W -> Dir.E\n\
}\n\
fun box(): String {\n\
    if (opposite(Dir.N) != Dir.S) return \"f1\"\n\
    if (opposite(Dir.W) != Dir.E) return \"f2\"\n\
    return \"OK\"\n\
}\n";
    run_ok(src, "EnumWhen");
}

#[test]
fn enum_reflection_members() {
    let src = "enum class Planet { MERCURY, VENUS, EARTH }\n\
fun box(): String {\n\
    if (Planet.values().size != 3) return \"f1\"\n\
    if (Planet.valueOf(\"EARTH\") != Planet.EARTH) return \"f2\"\n\
    if (Planet.EARTH.ordinal != 2) return \"f3\"\n\
    if (Planet.VENUS.name != \"VENUS\") return \"f4\"\n\
    if (Planet.entries.size != 3) return \"f5\"\n\
    return \"OK\"\n\
}\n";
    run_ok(src, "EnumMembers");
}

#[test]
fn enum_comparison_ordering() {
    let src = "enum class Level { LOW, MID, HIGH }\n\
fun box(): String {\n\
    if (!(Level.LOW < Level.HIGH)) return \"f1\"\n\
    if (Level.MID.compareTo(Level.LOW) <= 0) return \"f2\"\n\
    val list = listOf(Level.HIGH, Level.LOW, Level.MID).sorted()\n\
    if (list[0] != Level.LOW) return \"f3\"\n\
    if (list[2] != Level.HIGH) return \"f4\"\n\
    return \"OK\"\n\
}\n";
    run_ok(src, "EnumOrder");
}

#[test]
fn nested_sealed_hierarchy() {
    let src = "sealed class Node {\n\
    sealed class Leaf : Node()\n\
    data class IntLeaf(val v: Int) : Leaf()\n\
    data class StrLeaf(val v: String) : Leaf()\n\
    data class Branch(val l: Node, val r: Node) : Node()\n\
}\n\
fun count(n: Node): Int = when (n) {\n\
    is Node.IntLeaf -> 1\n\
    is Node.StrLeaf -> 1\n\
    is Node.Branch -> count(n.l) + count(n.r)\n\
}\n\
fun box(): String {\n\
    val t = Node.Branch(Node.IntLeaf(1), Node.StrLeaf(\"x\"))\n\
    if (count(t) != 2) return \"f1\"\n\
    return \"OK\"\n\
}\n";
    run_ok(src, "NestedSealed");
}

#[test]
fn sealed_with_generic_param() {
    let src = "sealed class Opt<out T>\n\
class Sm<T>(val value: T) : Opt<T>()\n\
class Nn : Opt<Nothing>()\n\
fun <T> unwrap(o: Opt<T>, dflt: T): T = when (o) {\n\
    is Sm -> o.value\n\
    is Nn -> dflt\n\
}\n\
fun box(): String {\n\
    if (unwrap(Sm(7), 0) != 7) return \"f1\"\n\
    if (unwrap(Nn(), 42) != 42) return \"f2\"\n\
    return \"OK\"\n\
}\n";
    run_ok(src, "SealedGeneric");
}

#[test]
fn data_class_many_fields_copy_destructure() {
    let src = "data class Rec(val a: Int, val b: String, val c: Boolean, val d: Int)\n\
fun box(): String {\n\
    val r = Rec(1, \"x\", true, 4)\n\
    if (r.component1() != 1) return \"f1\"\n\
    if (r.component2() != \"x\") return \"f2\"\n\
    if (r.component4() != 4) return \"f3\"\n\
    val r2 = r.copy(b = \"y\", d = 9)\n\
    if (r2.a != 1 || r2.b != \"y\" || r2.c != true || r2.d != 9) return \"f4\"\n\
    val (a, b, c, d) = r2\n\
    if (a != 1 || b != \"y\" || c != true || d != 9) return \"f5\"\n\
    return \"OK\"\n\
}\n";
    run_ok(src, "DataMany");
}

#[test]
fn data_class_equals_hashcode() {
    let src = "data class P(val x: Int, val y: Int)\n\
fun box(): String {\n\
    val a = P(1, 2)\n\
    val b = P(1, 2)\n\
    val c = P(3, 4)\n\
    if (a != b) return \"f1\"\n\
    if (a == c) return \"f2\"\n\
    if (a.hashCode() != b.hashCode()) return \"f3\"\n\
    return \"OK\"\n\
}\n";
    run_ok(src, "DataEq");
}

#[test]
fn data_class_map_key_and_sort() {
    let src = "data class Key(val id: Int, val tag: String)\n\
fun box(): String {\n\
    val m = HashMap<Key, Int>()\n\
    m[Key(1, \"a\")] = 10\n\
    m[Key(2, \"b\")] = 20\n\
    if (m[Key(1, \"a\")] != 10) return \"f1\"\n\
    if (m[Key(2, \"b\")] != 20) return \"f2\"\n\
    val sorted = listOf(Key(3, \"c\"), Key(1, \"a\"), Key(2, \"b\")).sortedBy { it.id }\n\
    if (sorted[0].id != 1 || sorted[2].id != 3) return \"f3\"\n\
    return \"OK\"\n\
}\n";
    run_ok(src, "DataMapKey");
}

#[test]
fn object_expression_multiple_supertypes() {
    let src = "interface A { fun a(): Int }\n\
interface B { fun b(): Int }\n\
fun make(): Any = object : A, B {\n\
    override fun a() = 1\n\
    override fun b() = 2\n\
}\n\
fun box(): String {\n\
    val o = make()\n\
    if ((o as A).a() != 1) return \"f1\"\n\
    if ((o as B).b() != 2) return \"f2\"\n\
    return \"OK\"\n\
}\n";
    run_ok(src, "ObjExprMulti");
}

#[test]
fn anonymous_object_with_state() {
    let src = "interface Counter { fun inc(): Int }\n\
fun make(): Counter = object : Counter {\n\
    var n = 0\n\
    override fun inc(): Int { n += 1; return n }\n\
}\n\
fun box(): String {\n\
    val c = make()\n\
    if (c.inc() != 1) return \"f1\"\n\
    if (c.inc() != 2) return \"f2\"\n\
    if (c.inc() != 3) return \"f3\"\n\
    return \"OK\"\n\
}\n";
    run_ok(src, "AnonState");
}

#[test]
fn companion_implementing_interface() {
    let src = "interface Factory { fun create(): String }\n\
class Widget {\n\
    companion object : Factory {\n\
        override fun create() = \"w\"\n\
    }\n\
}\n\
fun box(): String {\n\
    if (Widget.create() != \"w\") return \"f1\"\n\
    val f: Factory = Widget.Companion\n\
    if (f.create() != \"w\") return \"f2\"\n\
    return \"OK\"\n\
}\n";
    run_ok(src, "CompanionIface");
}

#[test]
fn companion_jvmstatic() {
    let src = "class Registry {\n\
    companion object {\n\
        @JvmStatic fun tag(): String = \"reg\"\n\
        @JvmStatic val version: Int = 3\n\
    }\n\
}\n\
fun box(): String {\n\
    if (Registry.tag() != \"reg\") return \"f1\"\n\
    if (Registry.version != 3) return \"f2\"\n\
    return \"OK\"\n\
}\n";
    run_ok(src, "CompanionStatic");
}

#[test]
fn typealias_in_signatures_and_bodies() {
    let src = "typealias IntList = List<Int>\n\
typealias Handler = (Int) -> Int\n\
fun sum(xs: IntList): Int {\n\
    var acc = 0\n\
    for (x in xs) acc += x\n\
    return acc\n\
}\n\
fun apply(h: Handler, v: Int): Int = h(v)\n\
fun box(): String {\n\
    val xs: IntList = listOf(1, 2, 3)\n\
    if (sum(xs) != 6) return \"f1\"\n\
    val doubler: Handler = { it * 2 }\n\
    if (apply(doubler, 5) != 10) return \"f2\"\n\
    return \"OK\"\n\
}\n";
    run_ok(src, "TypeAlias");
}
