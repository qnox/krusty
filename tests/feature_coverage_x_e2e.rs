//! End-to-end "box" coverage suite (x): exercises the checker/`types` resolution branches and the
//! Kotlin `@Metadata` signature emit/decode paths (`src/jvm/metadata.rs`) that model rich type
//! shapes — generic functions with bounds, declaration-site variance, nullable generics, function /
//! receiver / nested-generic types in signatures, default parameter values of many kinds, data-class
//! generated members (`equals`/`hashCode`/`toString`/`copy`/`componentN`), enums with per-entry
//! abstract overrides, sealed hierarchies (incl. generic), companion objects / `object` singletons,
//! interface default methods + diamond inheritance, `typealias`, and nested / `inner` classes.
//!
//! Two round-trip tests additionally compile a first source with krusty (emitting `@Metadata`), write
//! the classfiles to a classpath dir, then compile+run a SECOND source that references those
//! declarations — driving krusty's own metadata DECODE over metadata krusty itself EMITTED.

use super::common;

/// Single-compilation box run: everything lives in one source, cross-referencing declarations (which
/// still drives the checker/`types` resolution heavily).
fn run(src: &str, stem: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, stem)
}

/// Compile `lib_src` with krusty (emitting `@Metadata`), persist its classfiles to a fresh classpath
/// dir, then compile+run `main` against that dir — a genuine krusty-emit → krusty-decode round-trip.
fn roundtrip(tag: &str, lib_src: &str, main: &str) -> Option<String> {
    let sl = common::stdlib_jar()?;
    let jdk = common::jdk_modules()?;
    let lib_classes =
        common::compile_in_process(lib_src, "Lib", std::slice::from_ref(&sl), Some(&jdk))?;
    let dir = std::env::temp_dir().join(format!("krusty_cov10_{tag}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    for (name, bytes) in &lib_classes {
        let p = dir.join(format!("{name}.class"));
        std::fs::create_dir_all(p.parent()?).ok()?;
        std::fs::write(&p, bytes).ok()?;
    }
    common::compile_and_run_box(main, "Main", &[dir, sl], Some(&jdk))
}

// ---------------------------------------------------------------------------
// Rich generic signatures the checker / metadata must model.
// ---------------------------------------------------------------------------

#[test]
fn generic_fn_with_comparable_bound() {
    const SRC: &str = "fun <T : Comparable<T>> maxOfThree(a: T, b: T, c: T): T {\n\
    val m = if (a >= b) a else b\n\
    return if (m >= c) m else c\n\
}\n\
fun box(): String {\n\
    if (maxOfThree(3, 9, 5) != 9) return \"f1\"\n\
    if (maxOfThree(\"a\", \"z\", \"m\") != \"z\") return \"f2\"\n\
    return \"OK\"\n\
}\n";
    let Some(out) = run(SRC, "GenBound") else {
        eprintln!("skip");
        return;
    };
    assert_eq!(out, "OK");
}

#[test]
fn declaration_site_variance() {
    const SRC: &str = "class Producer<out T>(private val v: T) {\n\
    fun get(): T = v\n\
}\n\
class Consumer<in T> {\n\
    var last: String = \"\"\n\
    fun accept(x: T) { last = x.toString() }\n\
}\n\
fun readAny(p: Producer<Any>): String = p.get().toString()\n\
fun box(): String {\n\
    val ps: Producer<String> = Producer(\"hi\")\n\
    val pa: Producer<Any> = ps\n\
    if (readAny(pa) != \"hi\") return \"f1\"\n\
    val c: Consumer<String> = Consumer<Any>()\n\
    c.accept(\"x\")\n\
    if (c.last != \"x\") return \"f2\"\n\
    return \"OK\"\n\
}\n";
    let Some(out) = run(SRC, "Variance") else {
        eprintln!("skip");
        return;
    };
    assert_eq!(out, "OK");
}

#[test]
fn nullable_generic_params() {
    const SRC: &str = "fun <T> firstOrElse(xs: List<T?>, dflt: T): T {\n\
    for (x in xs) if (x != null) return x\n\
    return dflt\n\
}\n\
fun <T : Any> orNull(x: T?): T? = x\n\
fun box(): String {\n\
    val xs: List<Int?> = listOf(null, null, 7, 3)\n\
    if (firstOrElse(xs, -1) != 7) return \"f1\"\n\
    if (firstOrElse(listOf<String?>(null), \"d\") != \"d\") return \"f2\"\n\
    if (orNull<Int>(null) != null) return \"f3\"\n\
    if (orNull(5) != 5) return \"f4\"\n\
    return \"OK\"\n\
}\n";
    let Some(out) = run(SRC, "NullableGen") else {
        eprintln!("skip");
        return;
    };
    assert_eq!(out, "OK");
}

#[test]
fn function_types_as_params_and_returns() {
    const SRC: &str = "fun apply2(f: (Int) -> String, n: Int): String = f(n)\n\
fun adder(base: Int): (Int) -> Int = { x -> base + x }\n\
fun compose(f: (Int) -> Int, g: (Int) -> Int): (Int) -> Int = { x -> g(f(x)) }\n\
fun box(): String {\n\
    if (apply2({ it.toString() + \"!\" }, 4) != \"4!\") return \"f1\"\n\
    val add10 = adder(10)\n\
    if (add10(5) != 15) return \"f2\"\n\
    val h = compose({ it + 1 }, { it * 2 })\n\
    if (h(3) != 8) return \"f3\"\n\
    return \"OK\"\n\
}\n";
    let Some(out) = run(SRC, "FnTypes") else {
        eprintln!("skip");
        return;
    };
    assert_eq!(out, "OK");
}

#[test]
fn receiver_function_type_in_signature() {
    const SRC: &str = "fun <T, R> T.transform(block: T.() -> R): R = this.block()\n\
fun buildString2(block: StringBuilder.() -> Unit): String {\n\
    val sb = StringBuilder()\n\
    sb.block()\n\
    return sb.toString()\n\
}\n\
fun box(): String {\n\
    val r = 5.transform { this * this }\n\
    if (r != 25) return \"f1\"\n\
    val s = buildString2 { append(\"a\"); append(1) }\n\
    if (s != \"a1\") return \"f2\"\n\
    return \"OK\"\n\
}\n";
    let Some(out) = run(SRC, "RecvFnType") else {
        eprintln!("skip");
        return;
    };
    assert_eq!(out, "OK");
}

#[test]
fn deeply_nested_generic_signatures() {
    const SRC: &str =
        "fun <T> group(items: List<Pair<String, T>>): Map<String, List<Pair<Int, T>>> {\n\
    val out = HashMap<String, List<Pair<Int, T>>>()\n\
    var i = 0\n\
    for ((k, v) in items) {\n\
        out[k] = listOf(Pair(i, v))\n\
        i++\n\
    }\n\
    return out\n\
}\n\
fun box(): String {\n\
    val m = group(listOf(\"a\" to 10, \"b\" to 20))\n\
    val a = m[\"a\"]!![0]\n\
    if (a.first != 0 || a.second != 10) return \"f1\"\n\
    val b = m[\"b\"]!![0]\n\
    if (b.first != 1 || b.second != 20) return \"f2\"\n\
    return \"OK\"\n\
}\n";
    let Some(out) = run(SRC, "NestedGen") else {
        eprintln!("skip");
        return;
    };
    assert_eq!(out, "OK");
}

// ---------------------------------------------------------------------------
// Default parameter values of many kinds.
// ---------------------------------------------------------------------------

#[test]
fn default_params_many_kinds() {
    const SRC: &str = "fun render(\n\
    name: String,\n\
    count: Int = 1,\n\
    tag: String = \"t\",\n\
    note: String? = null,\n\
    extras: List<Int> = emptyList(),\n\
    fmt: (String) -> String = { it.uppercase() }\n\
): String {\n\
    var s = fmt(name) + \":\" + count + \":\" + tag\n\
    s += \":\" + (note ?: \"none\")\n\
    s += \":\" + extras.size\n\
    return s\n\
}\n\
fun box(): String {\n\
    if (render(\"a\") != \"A:1:t:none:0\") return \"f1|\" + render(\"a\")\n\
    if (render(\"a\", 3) != \"A:3:t:none:0\") return \"f2\"\n\
    if (render(\"a\", note = \"n\") != \"A:1:t:n:0\") return \"f3\"\n\
    if (render(\"a\", extras = listOf(1, 2)) != \"A:1:t:none:2\") return \"f4\"\n\
    if (render(\"a\", fmt = { it + \"!\" }) != \"a!:1:t:none:0\") return \"f5\"\n\
    return \"OK\"\n\
}\n";
    let Some(out) = run(SRC, "DefaultParams") else {
        eprintln!("skip");
        return;
    };
    assert_eq!(out, "OK");
}

#[test]
fn default_param_references_prior_param() {
    const SRC: &str = "fun span(start: Int, end: Int = start + 10, label: String = \"[\" + start + \",\" + end + \"]\"): String = label\n\
fun box(): String {\n\
    if (span(5) != \"[5,15]\") return \"f1|\" + span(5)\n\
    if (span(5, 8) != \"[5,8]\") return \"f2\"\n\
    if (span(5, 8, \"x\") != \"x\") return \"f3\"\n\
    return \"OK\"\n\
}\n";
    let Some(out) = run(SRC, "DefaultRefPrior") else {
        eprintln!("skip");
        return;
    };
    assert_eq!(out, "OK");
}

// ---------------------------------------------------------------------------
// Data class generated members.
// ---------------------------------------------------------------------------

#[test]
fn data_class_generated_members() {
    const SRC: &str = "data class User(val id: Int, val name: String, val active: Boolean, val score: Double)\n\
fun box(): String {\n\
    val u = User(1, \"ann\", true, 2.5)\n\
    val v = User(1, \"ann\", true, 2.5)\n\
    if (u != v) return \"f1\"\n\
    if (u.hashCode() != v.hashCode()) return \"f2\"\n\
    if (u.toString() != \"User(id=1, name=ann, active=true, score=2.5)\") return \"f3|\" + u.toString()\n\
    val w = u.copy(name = \"bob\", score = 9.0)\n\
    if (w.id != 1 || w.name != \"bob\" || !w.active || w.score != 9.0) return \"f4\"\n\
    if (u == w) return \"f5\"\n\
    val (id, name, active, score) = w\n\
    if (id != 1 || name != \"bob\" || !active || score != 9.0) return \"f6\"\n\
    if (u.component2() != \"ann\") return \"f7\"\n\
    return \"OK\"\n\
}\n";
    let Some(out) = run(SRC, "DataMembers") else {
        eprintln!("skip");
        return;
    };
    assert_eq!(out, "OK");
}

// ---------------------------------------------------------------------------
// Enum with properties, methods, per-entry abstract overrides.
// ---------------------------------------------------------------------------

#[test]
fn enum_rich_members() {
    const SRC: &str = "enum class Op(val symbol: String) {\n\
    ADD(\"+\") { override fun apply(a: Int, b: Int) = a + b },\n\
    MUL(\"*\") { override fun apply(a: Int, b: Int) = a * b };\n\
    abstract fun apply(a: Int, b: Int): Int\n\
    fun described(): String = symbol + \"=\" + name\n\
}\n\
fun box(): String {\n\
    if (Op.ADD.apply(2, 3) != 5) return \"f1\"\n\
    if (Op.MUL.apply(2, 3) != 6) return \"f2\"\n\
    if (Op.ADD.symbol != \"+\") return \"f3\"\n\
    if (Op.MUL.described() != \"*=MUL\") return \"f4\"\n\
    if (Op.ADD.ordinal != 0 || Op.MUL.ordinal != 1) return \"f5\"\n\
    if (Op.valueOf(\"MUL\") != Op.MUL) return \"f6\"\n\
    if (Op.values().size != 2) return \"f7\"\n\
    if (Op.entries.size != 2) return \"f8\"\n\
    if (Op.entries[0].name != \"ADD\") return \"f9\"\n\
    return \"OK\"\n\
}\n";
    let Some(out) = run(SRC, "EnumRich") else {
        eprintln!("skip");
        return;
    };
    assert_eq!(out, "OK");
}

// ---------------------------------------------------------------------------
// Sealed hierarchies (plain + generic) with exhaustive `when`.
// ---------------------------------------------------------------------------

#[test]
fn sealed_when_exhaustive() {
    const SRC: &str = "sealed interface Expr\n\
data class Lit(val v: Int) : Expr\n\
data class Neg(val e: Expr) : Expr\n\
data class Add(val l: Expr, val r: Expr) : Expr\n\
fun eval(e: Expr): Int = when (e) {\n\
    is Lit -> e.v\n\
    is Neg -> -eval(e.e)\n\
    is Add -> eval(e.l) + eval(e.r)\n\
}\n\
fun box(): String {\n\
    val e = Add(Lit(3), Neg(Lit(1)))\n\
    if (eval(e) != 2) return \"f1|\" + eval(e)\n\
    if (eval(Lit(9)) != 9) return \"f2\"\n\
    return \"OK\"\n\
}\n";
    let Some(out) = run(SRC, "SealedWhen") else {
        eprintln!("skip");
        return;
    };
    assert_eq!(out, "OK");
}

#[test]
fn sealed_generic_type_param() {
    const SRC: &str = "sealed class Res<out T>\n\
data class Ok<T>(val value: T) : Res<T>()\n\
data class Err(val msg: String) : Res<Nothing>()\n\
fun <T> unwrap(r: Res<T>, dflt: T): T = when (r) {\n\
    is Ok -> r.value\n\
    is Err -> dflt\n\
}\n\
fun box(): String {\n\
    val a: Res<Int> = Ok(42)\n\
    val b: Res<Int> = Err(\"bad\")\n\
    if (unwrap(a, -1) != 42) return \"f1\"\n\
    if (unwrap(b, -1) != -1) return \"f2\"\n\
    return \"OK\"\n\
}\n";
    let Some(out) = run(SRC, "SealedGeneric") else {
        eprintln!("skip");
        return;
    };
    assert_eq!(out, "OK");
}

// ---------------------------------------------------------------------------
// Companion objects + object singletons.
// ---------------------------------------------------------------------------

#[test]
fn companion_const_and_factory() {
    const SRC: &str = "class Temp private constructor(val celsius: Int) {\n\
    companion object {\n\
        const val ZERO_C: Int = 0\n\
        const val BOILING: Int = 100\n\
        fun freezing(): Temp = Temp(ZERO_C)\n\
        fun of(c: Int): Temp = Temp(c)\n\
    }\n\
    fun isBoiling(): Boolean = celsius >= BOILING\n\
}\n\
fun box(): String {\n\
    if (Temp.ZERO_C != 0) return \"f1\"\n\
    if (Temp.freezing().celsius != 0) return \"f2\"\n\
    if (!Temp.of(100).isBoiling()) return \"f3\"\n\
    if (Temp.of(50).isBoiling()) return \"f4\"\n\
    return \"OK\"\n\
}\n";
    let Some(out) = run(SRC, "Companion") else {
        eprintln!("skip");
        return;
    };
    assert_eq!(out, "OK");
}

#[test]
fn object_singleton_state() {
    const SRC: &str = "object Counter {\n\
    private var n: Int = 0\n\
    val label: String = \"counter\"\n\
    fun inc(): Int { n++; return n }\n\
    fun value(): Int = n\n\
}\n\
fun box(): String {\n\
    if (Counter.inc() != 1) return \"f1\"\n\
    if (Counter.inc() != 2) return \"f2\"\n\
    if (Counter.value() != 2) return \"f3\"\n\
    if (Counter.label != \"counter\") return \"f4\"\n\
    return \"OK\"\n\
}\n";
    let Some(out) = run(SRC, "ObjectSingleton") else {
        eprintln!("skip");
        return;
    };
    assert_eq!(out, "OK");
}

// ---------------------------------------------------------------------------
// Interfaces: default methods, generic, diamond inheritance.
// ---------------------------------------------------------------------------

#[test]
fn interface_default_methods() {
    const SRC: &str = "interface Greeter {\n\
    val who: String\n\
    fun name(): String\n\
    fun greet(): String = \"Hello, \" + name() + \" from \" + who\n\
}\n\
class Ann : Greeter {\n\
    override val who: String = \"HR\"\n\
    override fun name(): String = \"Ann\"\n\
}\n\
fun box(): String {\n\
    val g: Greeter = Ann()\n\
    if (g.greet() != \"Hello, Ann from HR\") return \"f1|\" + g.greet()\n\
    return \"OK\"\n\
}\n";
    let Some(out) = run(SRC, "IfaceDefault") else {
        eprintln!("skip");
        return;
    };
    assert_eq!(out, "OK");
}

#[test]
fn generic_interface_impl() {
    const SRC: &str = "interface Container<T> {\n\
    fun put(x: T)\n\
    fun get(): T\n\
    fun mapToString(): String = get().toString()\n\
}\n\
class Cell<T>(private var v: T) : Container<T> {\n\
    override fun put(x: T) { v = x }\n\
    override fun get(): T = v\n\
}\n\
fun box(): String {\n\
    val c: Container<Int> = Cell(1)\n\
    c.put(7)\n\
    if (c.get() != 7) return \"f1\"\n\
    if (c.mapToString() != \"7\") return \"f2\"\n\
    return \"OK\"\n\
}\n";
    let Some(out) = run(SRC, "GenIface") else {
        eprintln!("skip");
        return;
    };
    assert_eq!(out, "OK");
}

#[test]
fn interface_diamond_inheritance() {
    const SRC: &str = "interface A { fun tag(): String = \"A\" }\n\
interface B : A { fun extra(): String = \"B\" }\n\
interface C : A { fun other(): String = \"C\" }\n\
class D : B, C { override fun tag(): String = \"D:\" + extra() + other() }\n\
fun box(): String {\n\
    val d: A = D()\n\
    if (d.tag() != \"D:BC\") return \"f1|\" + d.tag()\n\
    return \"OK\"\n\
}\n";
    let Some(out) = run(SRC, "Diamond") else {
        eprintln!("skip");
        return;
    };
    assert_eq!(out, "OK");
}

// ---------------------------------------------------------------------------
// Type aliases in signatures.
// ---------------------------------------------------------------------------

#[test]
fn typealias_function_and_generic() {
    const SRC: &str = "typealias IntOp = (Int, Int) -> Int\n\
typealias Table<V> = Map<String, V>\n\
fun fold(xs: List<Int>, seed: Int, op: IntOp): Int {\n\
    var acc = seed\n\
    for (x in xs) acc = op(acc, x)\n\
    return acc\n\
}\n\
fun lookup(t: Table<Int>, key: String): Int = t[key] ?: -1\n\
fun box(): String {\n\
    if (fold(listOf(1, 2, 3), 0, { a, b -> a + b }) != 6) return \"f1\"\n\
    val t: Table<Int> = mapOf(\"a\" to 1)\n\
    if (lookup(t, \"a\") != 1) return \"f2\"\n\
    if (lookup(t, \"z\") != -1) return \"f3\"\n\
    return \"OK\"\n\
}\n";
    let Some(out) = run(SRC, "TypeAlias") else {
        eprintln!("skip");
        return;
    };
    assert_eq!(out, "OK");
}

// ---------------------------------------------------------------------------
// Nested & inner classes.
// ---------------------------------------------------------------------------

#[test]
fn nested_and_inner_classes() {
    const SRC: &str = "class Outer(val base: Int) {\n\
    class Nested(val k: Int) {\n\
        fun doubled(): Int = k * 2\n\
    }\n\
    inner class Inner(val add: Int) {\n\
        fun total(): Int = base + add\n\
    }\n\
    fun make(a: Int): Inner = Inner(a)\n\
}\n\
fun box(): String {\n\
    val n = Outer.Nested(5)\n\
    if (n.doubled() != 10) return \"f1\"\n\
    val o = Outer(100)\n\
    val i = o.Inner(7)\n\
    if (i.total() != 107) return \"f2\"\n\
    if (o.make(3).total() != 103) return \"f3\"\n\
    return \"OK\"\n\
}\n";
    let Some(out) = run(SRC, "NestedInner") else {
        eprintln!("skip");
        return;
    };
    assert_eq!(out, "OK");
}

// ---------------------------------------------------------------------------
// krusty-emit → krusty-decode @Metadata round-trips (two compilations).
// ---------------------------------------------------------------------------

#[test]
fn roundtrip_data_class_and_generic_fn() {
    const LIB: &str = "data class Point(val x: Int, val y: Int) {\n\
    fun manhattan(): Int = kotlin.math.abs(x) + kotlin.math.abs(y)\n\
}\n\
fun <T : Comparable<T>> clampMax(v: T, hi: T): T = if (v >= hi) hi else v\n";
    const MAIN: &str = "fun box(): String {\n\
    val p = Point(3, -4)\n\
    if (p.manhattan() != 7) return \"f1\"\n\
    val q = p.copy(y = 4)\n\
    if (q.y != 4 || q.x != 3) return \"f2\"\n\
    val (a, b) = q\n\
    if (a != 3 || b != 4) return \"f3\"\n\
    if (clampMax(10, 7) != 7) return \"f4\"\n\
    if (clampMax(\"a\", \"z\") != \"a\") return \"f5\"\n\
    return \"OK\"\n\
}\n";
    let Some(out) = roundtrip("data", LIB, MAIN) else {
        eprintln!("skip");
        return;
    };
    assert_eq!(out, "OK");
}

#[test]
fn roundtrip_enum_sealed_and_iface() {
    const LIB: &str = "enum class Color(val hex: String) { RED(\"f00\"), GREEN(\"0f0\") }\n\
sealed interface Shape\n\
data class Circle(val r: Int) : Shape\n\
data class Rect(val w: Int, val h: Int) : Shape\n\
interface Area { fun area(): Int }\n\
fun areaOf(s: Shape): Int = when (s) {\n\
    is Circle -> 3 * s.r * s.r\n\
    is Rect -> s.w * s.h\n\
}\n";
    const MAIN: &str = "fun box(): String {\n\
    if (Color.RED.hex != \"f00\") return \"f1\"\n\
    if (Color.valueOf(\"GREEN\").ordinal != 1) return \"f2\"\n\
    if (Color.values().size != 2) return \"f3\"\n\
    val s: Shape = Rect(2, 3)\n\
    if (areaOf(s) != 6) return \"f4\"\n\
    if (areaOf(Circle(2)) != 12) return \"f5\"\n\
    return \"OK\"\n\
}\n";
    let Some(out) = roundtrip("enum", LIB, MAIN) else {
        eprintln!("skip");
        return;
    };
    assert_eq!(out, "OK");
}
