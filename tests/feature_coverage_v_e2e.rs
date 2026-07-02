//! End-to-end "box" coverage suite (v): exercises less-common lowering/emit/resolve branches not
//! covered by earlier suites — data-class destructuring (including nested), value-producing `when`
//! forms, try/catch/finally control-flow interplay, char/long ranges with step/downTo/until, string
//! helpers (trimIndent/trimMargin/repeat/chunked/format), nested generic collections, receiver
//! lambdas / apply-also DSL builders, user-defined operators (get/set/invoke/contains/rangeTo/
//! unaryMinus/compareTo/plus/times), lateinit + `::prop.isInitialized`, generic companion factories,
//! nested recursion, and vararg forwarding via spread. Each test compiles a `fun box(): String`
//! returning "OK" and round-trips it on the JVM.

mod common;

fn run(src: &str, stem: &str) -> Option<String> {
    let jh = common::java_home()?;
    let sl = common::stdlib_jar()?;
    let jdk = std::path::PathBuf::from(format!("{jh}/lib/modules"));
    common::compile_and_run_box(src, stem, &[sl], Some(&jdk))
}

#[test]
fn destructure_data_class_from_fn() {
    const SRC: &str = "data class Pt(val x: Int, val y: Int)\n\
fun mk(): Pt = Pt(3, 4)\n\
fun box(): String {\n\
    val (a, b) = mk()\n\
    if (a != 3) return \"a=$a\"\n\
    if (b != 4) return \"b=$b\"\n\
    return \"OK\"\n\
}\n";
    let Some(out) = run(SRC, "DestrDataFn") else {
        eprintln!("skip");
        return;
    };
    assert_eq!(out, "OK");
}

#[test]
fn destructure_nested_in_loop() {
    const SRC: &str = "data class Pair2(val a: Int, val b: String)\n\
fun box(): String {\n\
    val xs = listOf(Pair2(1, \"a\"), Pair2(2, \"b\"))\n\
    var sum = 0\n\
    var str = \"\"\n\
    for ((n, s) in xs) { sum += n; str += s }\n\
    if (sum != 3) return \"sum=$sum\"\n\
    if (str != \"ab\") return \"str=$str\"\n\
    return \"OK\"\n\
}\n";
    let Some(out) = run(SRC, "DestrLoop") else {
        eprintln!("skip");
        return;
    };
    assert_eq!(out, "OK");
}

#[test]
fn when_value_multi_type() {
    const SRC: &str = "fun classify(x: Any): String = when (x) {\n\
    is Int -> \"int:\" + x.toString()\n\
    is String -> \"str:\" + x\n\
    else -> \"other\"\n\
}\n\
fun box(): String {\n\
    if (classify(5) != \"int:5\") return \"f1\"\n\
    if (classify(\"hi\") != \"str:hi\") return \"f2\"\n\
    if (classify(3.0) != \"other\") return \"f3\"\n\
    return \"OK\"\n\
}\n";
    let Some(out) = run(SRC, "WhenMultiType") else {
        eprintln!("skip");
        return;
    };
    assert_eq!(out, "OK");
}

#[test]
fn when_sealed_with_guard() {
    const SRC: &str = "sealed class Shape\n\
class Circle(val r: Int) : Shape()\n\
class Square(val s: Int) : Shape()\n\
fun describe(sh: Shape): String = when (sh) {\n\
    is Circle -> if (sh.r > 10) \"big-circle\" else \"circle\"\n\
    is Square -> if (sh.s > 10) \"big-square\" else \"square\"\n\
}\n\
fun box(): String {\n\
    if (describe(Circle(5)) != \"circle\") return \"f1\"\n\
    if (describe(Circle(20)) != \"big-circle\") return \"f2\"\n\
    if (describe(Square(3)) != \"square\") return \"f3\"\n\
    return \"OK\"\n\
}\n";
    let Some(out) = run(SRC, "WhenSealedGuard") else {
        eprintln!("skip");
        return;
    };
    assert_eq!(out, "OK");
}

#[test]
fn try_finally_alters_control() {
    const SRC: &str = "fun compute(): Int {\n\
    var r = 0\n\
    try {\n\
        r = 1\n\
        return r\n\
    } finally {\n\
        r = 2\n\
    }\n\
}\n\
fun tryCatchVal(b: Boolean): Int {\n\
    val v = try { if (b) throw RuntimeException(\"x\") else 10 } catch (e: RuntimeException) { 20 }\n\
    return v\n\
}\n\
fun box(): String {\n\
    if (compute() != 1) return \"f1\"\n\
    if (tryCatchVal(false) != 10) return \"f2\"\n\
    if (tryCatchVal(true) != 20) return \"f3\"\n\
    return \"OK\"\n\
}\n";
    let Some(out) = run(SRC, "TryFinallyCtl") else {
        eprintln!("skip");
        return;
    };
    assert_eq!(out, "OK");
}

#[test]
fn multi_catch_hierarchies() {
    const SRC: &str = "fun handle(kind: Int): String {\n\
    return try {\n\
        when (kind) {\n\
            0 -> throw IllegalArgumentException(\"a\")\n\
            1 -> throw IllegalStateException(\"b\")\n\
            else -> throw RuntimeException(\"c\")\n\
        }\n\
    } catch (e: IllegalArgumentException) {\n\
        \"iae\"\n\
    } catch (e: IllegalStateException) {\n\
        \"ise\"\n\
    } catch (e: RuntimeException) {\n\
        \"re\"\n\
    }\n\
}\n\
fun box(): String {\n\
    if (handle(0) != \"iae\") return \"f1\"\n\
    if (handle(1) != \"ise\") return \"f2\"\n\
    if (handle(2) != \"re\") return \"f3\"\n\
    return \"OK\"\n\
}\n";
    let Some(out) = run(SRC, "MultiCatch") else {
        eprintln!("skip");
        return;
    };
    assert_eq!(out, "OK");
}

#[test]
fn char_range_and_contains() {
    const SRC: &str = "fun box(): String {\n\
    var count = 0\n\
    for (c in 'a'..'e') count++\n\
    if (count != 5) return \"count=$count\"\n\
    if ('c' !in 'a'..'z') return \"f2\"\n\
    if ('C' in 'a'..'z') return \"f3\"\n\
    val down = StringBuilder()\n\
    for (c in 'e' downTo 'a') down.append(c)\n\
    if (down.toString() != \"edcba\") return \"down=$down\"\n\
    return \"OK\"\n\
}\n";
    let Some(out) = run(SRC, "CharRange") else {
        eprintln!("skip");
        return;
    };
    assert_eq!(out, "OK");
}

#[test]
fn long_range_step_until() {
    const SRC: &str = "fun box(): String {\n\
    var s = 0L\n\
    for (i in 0L..10L step 2L) s += i\n\
    if (s != 30L) return \"s=$s\"\n\
    var u = 0\n\
    for (i in 0 until 5) u += i\n\
    if (u != 10) return \"u=$u\"\n\
    var d = 0\n\
    for (i in 10 downTo 1 step 3) d += i\n\
    if (d != 22) return \"d=$d\"\n\
    if (7L !in 0L..10L) return \"f4\"\n\
    return \"OK\"\n\
}\n";
    let Some(out) = run(SRC, "LongRangeStep") else {
        eprintln!("skip");
        return;
    };
    assert_eq!(out, "OK");
}

#[test]
fn string_trim_and_repeat() {
    const SRC: &str = "fun box(): String {\n\
    val t = \"\"\"\n\
        line1\n\
        line2\n\
    \"\"\".trimIndent()\n\
    if (t != \"line1\\nline2\") return \"t=[$t]\"\n\
    val m = \"\"\"\n\
        |a\n\
        |b\n\
    \"\"\".trimMargin()\n\
    if (m != \"a\\nb\") return \"m=[$m]\"\n\
    if (\"ab\".repeat(3) != \"ababab\") return \"f3\"\n\
    return \"OK\"\n\
}\n";
    let Some(out) = run(SRC, "StringTrim") else {
        eprintln!("skip");
        return;
    };
    assert_eq!(out, "OK");
}

#[test]
fn string_chunked_and_compare() {
    const SRC: &str = "fun box(): String {\n\
    val parts = \"abcdef\".chunked(2)\n\
    if (parts.size != 3) return \"size=${parts.size}\"\n\
    if (parts[0] != \"ab\" || parts[2] != \"ef\") return \"f2\"\n\
    if (!(\"apple\" < \"banana\")) return \"f3\"\n\
    if (\"zed\" <= \"abc\") return \"f4\"\n\
    val n = 42\n\
    val fs = \"n=$n\"\n\
    if (fs != \"n=42\") return \"f5\"\n\
    return \"OK\"\n\
}\n";
    let Some(out) = run(SRC, "StringChunked") else {
        eprintln!("skip");
        return;
    };
    assert_eq!(out, "OK");
}

#[test]
fn map_of_list_values() {
    const SRC: &str = "fun box(): String {\n\
    val m: Map<String, List<Int>> = mapOf(\"a\" to listOf(1, 2), \"b\" to listOf(3))\n\
    var sum = 0\n\
    for ((k, vs) in m) { for (v in vs) sum += v }\n\
    if (sum != 6) return \"sum=$sum\"\n\
    val a = m[\"a\"]!!\n\
    if (a.size != 2) return \"asize=${a.size}\"\n\
    return \"OK\"\n\
}\n";
    let Some(out) = run(SRC, "MapOfList") else {
        eprintln!("skip");
        return;
    };
    assert_eq!(out, "OK");
}

#[test]
fn list_of_pairs_nested_generics() {
    const SRC: &str = "fun box(): String {\n\
    val xs: List<Pair<Int, String>> = listOf(1 to \"one\", 2 to \"two\")\n\
    var acc = \"\"\n\
    for (p in xs) acc += \"${p.first}:${p.second};\"\n\
    if (acc != \"1:one;2:two;\") return \"acc=$acc\"\n\
    val m: Map<Int, List<Pair<Int, Int>>> = mapOf(1 to listOf(1 to 2, 3 to 4))\n\
    val inner = m[1]!!\n\
    if (inner[1].second != 4) return \"f2\"\n\
    return \"OK\"\n\
}\n";
    let Some(out) = run(SRC, "ListOfPairs") else {
        eprintln!("skip");
        return;
    };
    assert_eq!(out, "OK");
}

#[test]
fn receiver_lambda_and_dsl_builder() {
    const SRC: &str = "class Cfg {\n\
    var name: String = \"\"\n\
    var count: Int = 0\n\
}\n\
fun build(block: Cfg.() -> Unit): Cfg {\n\
    val c = Cfg()\n\
    c.block()\n\
    return c\n\
}\n\
fun box(): String {\n\
    val c = build {\n\
        name = \"x\"\n\
        count = 5\n\
    }\n\
    if (c.name != \"x\") return \"f1\"\n\
    if (c.count != 5) return \"f2\"\n\
    val sb = StringBuilder().apply { append(\"a\"); append(\"b\") }.also { it.append(\"c\") }\n\
    if (sb.toString() != \"abc\") return \"f3\"\n\
    return \"OK\"\n\
}\n";
    let Some(out) = run(SRC, "ReceiverDsl") else {
        eprintln!("skip");
        return;
    };
    assert_eq!(out, "OK");
}

#[test]
fn operators_get_set_invoke_contains() {
    const SRC: &str = "class Grid {\n\
    val data = IntArray(4)\n\
    operator fun get(i: Int): Int = data[i]\n\
    operator fun set(i: Int, v: Int) { data[i] = v }\n\
    operator fun contains(v: Int): Boolean { for (x in data) if (x == v) return true; return false }\n\
}\n\
class Adder(val base: Int) {\n\
    operator fun invoke(x: Int): Int = base + x\n\
}\n\
fun box(): String {\n\
    val g = Grid()\n\
    g[0] = 10\n\
    g[1] = 20\n\
    if (g[0] != 10) return \"f1\"\n\
    if (g[1] != 20) return \"f2\"\n\
    if (20 !in g) return \"f3\"\n\
    if (99 in g) return \"f4\"\n\
    val add = Adder(100)\n\
    if (add(5) != 105) return \"f5\"\n\
    return \"OK\"\n\
}\n";
    let Some(out) = run(SRC, "OpsGetSet") else {
        eprintln!("skip");
        return;
    };
    assert_eq!(out, "OK");
}

#[test]
fn operators_arithmetic_compare_range() {
    const SRC: &str = "data class V2(val x: Int, val y: Int) {\n\
    operator fun plus(o: V2) = V2(x + o.x, y + o.y)\n\
    operator fun times(k: Int) = V2(x * k, y * k)\n\
    operator fun unaryMinus() = V2(-x, -y)\n\
    operator fun compareTo(o: V2): Int = (x + y) - (o.x + o.y)\n\
}\n\
fun box(): String {\n\
    val a = V2(1, 2)\n\
    val b = V2(3, 4)\n\
    if (a + b != V2(4, 6)) return \"f1\"\n\
    if (a * 3 != V2(3, 6)) return \"f2\"\n\
    if (-a != V2(-1, -2)) return \"f3\"\n\
    if (!(a < b)) return \"f4\"\n\
    if (b <= a) return \"f5\"\n\
    return \"OK\"\n\
}\n";
    let Some(out) = run(SRC, "OpsArith") else {
        eprintln!("skip");
        return;
    };
    assert_eq!(out, "OK");
}

#[test]
fn operator_rangeto_user_type() {
    const SRC: &str = "class Ver(val n: Int) {\n\
    operator fun rangeTo(o: Ver): VerRange = VerRange(n, o.n)\n\
}\n\
class VerRange(val lo: Int, val hi: Int) {\n\
    operator fun contains(v: Int): Boolean = v in lo..hi\n\
}\n\
fun box(): String {\n\
    val r = Ver(1)..Ver(5)\n\
    if (3 !in r) return \"f1\"\n\
    if (9 in r) return \"f2\"\n\
    if (r.lo != 1 || r.hi != 5) return \"f3\"\n\
    return \"OK\"\n\
}\n";
    let Some(out) = run(SRC, "OpsRangeTo") else {
        eprintln!("skip");
        return;
    };
    assert_eq!(out, "OK");
}

#[test]
fn lateinit_and_isinitialized() {
    const SRC: &str = "class Holder {\n\
    lateinit var name: String\n\
    fun ready(): Boolean = ::name.isInitialized\n\
    fun init() { name = \"set\" }\n\
}\n\
fun box(): String {\n\
    val h = Holder()\n\
    if (h.ready()) return \"f1\"\n\
    h.init()\n\
    if (!h.ready()) return \"f2\"\n\
    if (h.name != \"set\") return \"f3\"\n\
    return \"OK\"\n\
}\n";
    let Some(out) = run(SRC, "Lateinit") else {
        eprintln!("skip");
        return;
    };
    assert_eq!(out, "OK");
}

#[test]
fn generic_companion_factory() {
    const SRC: &str = "class Box<T>(val value: T) {\n\
    companion object {\n\
        fun <T> of(v: T): Box<T> = Box(v)\n\
    }\n\
}\n\
fun <T> firstOr(list: List<T>, dflt: T): T = if (list.isEmpty()) dflt else list[0]\n\
fun box(): String {\n\
    val b = Box.of(42)\n\
    if (b.value != 42) return \"f1\"\n\
    val s = Box.of(\"hi\")\n\
    if (s.value != \"hi\") return \"f2\"\n\
    if (firstOr(listOf(1, 2), 0) != 1) return \"f3\"\n\
    if (firstOr(listOf<Int>(), 9) != 9) return \"f4\"\n\
    return \"OK\"\n\
}\n";
    let Some(out) = run(SRC, "GenericFactory") else {
        eprintln!("skip");
        return;
    };
    assert_eq!(out, "OK");
}

#[test]
fn nested_recursion() {
    const SRC: &str = "fun box(): String {\n\
    fun fib(n: Int): Int = if (n < 2) n else fib(n - 1) + fib(n - 2)\n\
    if (fib(10) != 55) return \"f1\"\n\
    var captured = 0\n\
    fun accumulate(n: Int) { if (n > 0) { captured += n; accumulate(n - 1) } }\n\
    accumulate(4)\n\
    if (captured != 10) return \"cap=$captured\"\n\
    return \"OK\"\n\
}\n";
    let Some(out) = run(SRC, "NestedRecursion") else {
        eprintln!("skip");
        return;
    };
    assert_eq!(out, "OK");
}

#[test]
fn vararg_spread_forwarding() {
    const SRC: &str =
        "fun total(vararg ns: Int): Int { var s = 0; for (n in ns) s += n; return s }\n\
fun forward(vararg ns: Int): Int = total(*ns, 100)\n\
fun labeled(prefix: String, vararg parts: String): String {\n\
    var r = prefix\n\
    for (p in parts) r += p\n\
    return r\n\
}\n\
fun box(): String {\n\
    if (total(1, 2, 3) != 6) return \"f1\"\n\
    if (forward(1, 2, 3) != 106) return \"f2\"\n\
    if (labeled(\"x\", \"a\", \"b\") != \"xab\") return \"f3\"\n\
    if (labeled(prefix = \"p\") != \"p\") return \"f4\"\n\
    return \"OK\"\n\
}\n";
    let Some(out) = run(SRC, "VarargSpread") else {
        eprintln!("skip");
        return;
    };
    assert_eq!(out, "OK");
}

#[test]
fn named_default_vararg_mix() {
    const SRC: &str = "fun make(name: String, factor: Int = 2, vararg extra: Int): Int {\n\
    var s = name.length * factor\n\
    for (e in extra) s += e\n\
    return s\n\
}\n\
fun box(): String {\n\
    if (make(\"ab\") != 4) return \"f1\"\n\
    if (make(\"ab\", 3) != 6) return \"f2\"\n\
    if (make(\"ab\", 3, 1, 2) != 9) return \"f3\"\n\
    return \"OK\"\n\
}\n";
    let Some(out) = run(SRC, "NamedDefaultVararg") else {
        eprintln!("skip");
        return;
    };
    assert_eq!(out, "OK");
}
