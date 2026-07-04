//! End-to-end "box" coverage for exceptions, try-as-expression, `runCatching`/`Result`,
//! generics (bounds, `where`, multiple type params, explicit type args), variance, star
//! projection, reified inline, and nested generics. Each test compiles a `fun box(): String`
//! that returns "OK", then runs it on the JVM. Round-tripped against the real runtime.

use super::common;

/// Shared harness: compile `src`'s `box()` under `stem` and assert it returns "OK".
/// Returns without asserting (skips) when the toolchain/stdlib/JDK is unavailable.
fn run(src: &str, stem: &str) {
    common::assert_box_ok_with_stdlib(src, stem);
}

// --- exceptions ---------------------------------------------------------------------------

#[test]
fn custom_exception_message() {
    let src = "class MyError(msg: String) : RuntimeException(msg)\n\
fun box(): String {\n\
    try {\n\
        throw MyError(\"boom\")\n\
    } catch (e: MyError) {\n\
        if (e.message != \"boom\") return \"f1\"\n\
    }\n\
    return \"OK\"\n\
}\n";
    run(src, "CustomExc");
}

#[test]
fn exception_hierarchy_catch_base() {
    let src = "open class Base(msg: String) : RuntimeException(msg)\n\
class Derived(msg: String) : Base(msg)\n\
fun box(): String {\n\
    try {\n\
        throw Derived(\"d\")\n\
    } catch (e: Base) {\n\
        if (e.message != \"d\") return \"f1\"\n\
        return \"OK\"\n\
    }\n\
    return \"f2\"\n\
}\n";
    run(src, "ExcHier");
}

#[test]
fn finally_runs_both_paths() {
    let src = "fun run1(fail: Boolean, log: StringBuilder): Int {\n\
    try {\n\
        if (fail) throw RuntimeException(\"x\")\n\
        return 1\n\
    } catch (e: RuntimeException) {\n\
        return 2\n\
    } finally {\n\
        log.append(\"F\")\n\
    }\n\
}\n\
fun box(): String {\n\
    val log = StringBuilder()\n\
    if (run1(false, log) != 1) return \"f1\"\n\
    if (run1(true, log) != 2) return \"f2\"\n\
    if (log.toString() != \"FF\") return \"f3\"\n\
    return \"OK\"\n\
}\n";
    run(src, "FinallyBoth");
}

#[test]
fn return_inside_try_with_finally() {
    let src = "fun compute(log: StringBuilder): Int {\n\
    try {\n\
        return 42\n\
    } finally {\n\
        log.append(\"F\")\n\
    }\n\
}\n\
fun box(): String {\n\
    val log = StringBuilder()\n\
    if (compute(log) != 42) return \"f1\"\n\
    if (log.toString() != \"F\") return \"f2\"\n\
    return \"OK\"\n\
}\n";
    run(src, "RetTryFinally");
}

#[test]
fn rethrow_in_catch() {
    let src = "fun inner() { throw IllegalStateException(\"bad\") }\n\
fun box(): String {\n\
    try {\n\
        try {\n\
            inner()\n\
        } catch (e: IllegalStateException) {\n\
            throw RuntimeException(\"wrapped\")\n\
        }\n\
    } catch (e: RuntimeException) {\n\
        if (e.message != \"wrapped\") return \"f1\"\n\
        return \"OK\"\n\
    }\n\
    return \"f2\"\n\
}\n";
    run(src, "Rethrow");
}

#[test]
fn nested_try_catch() {
    let src = "fun box(): String {\n\
    var hits = 0\n\
    try {\n\
        try {\n\
            throw IllegalArgumentException(\"a\")\n\
        } catch (e: IllegalArgumentException) {\n\
            hits += 1\n\
            throw IllegalStateException(\"b\")\n\
        }\n\
    } catch (e: IllegalStateException) {\n\
        hits += 10\n\
    }\n\
    if (hits != 11) return \"f1\"\n\
    return \"OK\"\n\
}\n";
    run(src, "NestedTry");
}

// --- try as an expression -----------------------------------------------------------------

#[test]
fn try_as_expression() {
    let src =
        "fun parse(s: String): Int = try { s.toInt() } catch (e: NumberFormatException) { -1 }\n\
fun box(): String {\n\
    if (parse(\"12\") != 12) return \"f1\"\n\
    if (parse(\"nope\") != -1) return \"f2\"\n\
    return \"OK\"\n\
}\n";
    run(src, "TryExpr");
}

// --- runCatching / Result -----------------------------------------------------------------

#[test]
fn run_catching_get_or_else() {
    let src = "fun box(): String {\n\
    val ok = runCatching { 10 / 2 }.getOrElse { -1 }\n\
    if (ok != 5) return \"f1\"\n\
    val bad = runCatching { 10 / 0 }.getOrElse { -1 }\n\
    if (bad != -1) return \"f2\"\n\
    return \"OK\"\n\
}\n";
    run(src, "RunCatchingElse");
}

#[test]
fn run_catching_get_or_null() {
    let src = "fun box(): String {\n\
    val ok = runCatching { \"7\".toInt() }.getOrNull()\n\
    if (ok != 7) return \"f1\"\n\
    val bad = runCatching { \"x\".toInt() }.getOrNull()\n\
    if (bad != null) return \"f2\"\n\
    return \"OK\"\n\
}\n";
    run(src, "RunCatchingNull");
}

#[test]
fn result_is_success() {
    let src = "fun box(): String {\n\
    val r: Result<Int> = runCatching { 3 + 4 }\n\
    if (!r.isSuccess) return \"f1\"\n\
    if (r.getOrThrow() != 7) return \"f2\"\n\
    val f: Result<Int> = runCatching { throw RuntimeException(\"e\") }\n\
    if (!f.isFailure) return \"f3\"\n\
    return \"OK\"\n\
}\n";
    run(src, "ResultType");
}

// --- generics -----------------------------------------------------------------------------

#[test]
fn bounded_type_param_comparable() {
    let src = "fun <T : Comparable<T>> maxOf2(a: T, b: T): T = if (a >= b) a else b\n\
fun box(): String {\n\
    if (maxOf2(3, 8) != 8) return \"f1\"\n\
    if (maxOf2(\"apple\", \"banana\") != \"banana\") return \"f2\"\n\
    return \"OK\"\n\
}\n";
    run(src, "BoundedCmp");
}

#[test]
fn where_clause_two_bounds() {
    let src = "interface Named { val name: String }\n\
fun <T> label(x: T): String where T : Comparable<T>, T : Named = x.name\n\
class Item(override val name: String) : Comparable<Item>, Named {\n\
    override fun compareTo(other: Item): Int = name.compareTo(other.name)\n\
}\n\
fun box(): String {\n\
    if (label(Item(\"widget\")) != \"widget\") return \"f1\"\n\
    return \"OK\"\n\
}\n";
    run(src, "WhereClause");
}

#[test]
fn generic_class_multiple_params() {
    let src = "class Pair2<A, B>(val first: A, val second: B) {\n\
    fun swap(): Pair2<B, A> = Pair2(second, first)\n\
}\n\
fun box(): String {\n\
    val p = Pair2(1, \"a\")\n\
    val q = p.swap()\n\
    if (q.first != \"a\") return \"f1\"\n\
    if (q.second != 1) return \"f2\"\n\
    return \"OK\"\n\
}\n";
    run(src, "GenMultiParam");
}

#[test]
fn generic_function_explicit_type_args() {
    let src = "fun <T> singletonList(x: T): List<T> = listOf(x)\n\
fun box(): String {\n\
    val xs = singletonList<Int>(5)\n\
    if (xs.size != 1) return \"f1\"\n\
    if (xs[0] != 5) return \"f2\"\n\
    return \"OK\"\n\
}\n";
    run(src, "ExplicitTypeArgs");
}

// --- variance -----------------------------------------------------------------------------

#[test]
fn variance_out_and_in() {
    let src = "class Box<out T>(val value: T)\n\
fun produce(): Box<Any> {\n\
    val b: Box<String> = Box(\"hi\")\n\
    return b\n\
}\n\
fun consume(c: Comparable<Int>): Int = c.compareTo(3)\n\
fun box(): String {\n\
    if (produce().value != \"hi\") return \"f1\"\n\
    val n: Comparable<Int> = 3\n\
    if (consume(n) != 0) return \"f2\"\n\
    return \"OK\"\n\
}\n";
    run(src, "Variance");
}

#[test]
fn star_projection() {
    let src = "fun total(lists: List<List<*>>): Int {\n\
    var n = 0\n\
    for (l in lists) n += l.size\n\
    return n\n\
}\n\
fun box(): String {\n\
    val a: List<*> = listOf(1, 2, 3)\n\
    val b: List<*> = listOf(\"x\")\n\
    if (a.size != 3) return \"f1\"\n\
    if (total(listOf(a, b)) != 4) return \"f2\"\n\
    return \"OK\"\n\
}\n";
    run(src, "StarProj");
}

// --- reified inline -----------------------------------------------------------------------

#[test]
fn reified_is_check() {
    let src = "inline fun <reified T> countOf(xs: List<Any>): Int {\n\
    var n = 0\n\
    for (x in xs) if (x is T) n += 1\n\
    return n\n\
}\n\
fun box(): String {\n\
    val xs = listOf(1, \"a\", 2, \"b\", 3)\n\
    if (countOf<Int>(xs) != 3) return \"f1\"\n\
    if (countOf<String>(xs) != 2) return \"f2\"\n\
    return \"OK\"\n\
}\n";
    run(src, "ReifiedIs");
}

#[test]
fn reified_class_name() {
    let src = "inline fun <reified T> nameOf(): String = T::class.java.simpleName\n\
fun box(): String {\n\
    if (nameOf<String>() != \"String\") return \"f1\"\n\
    return \"OK\"\n\
}\n";
    run(src, "ReifiedClass");
}

// --- nested generics ----------------------------------------------------------------------

#[test]
fn nested_generic_map() {
    let src = "fun box(): String {\n\
    val m: Map<String, List<Int>> = mapOf(\"a\" to listOf(1, 2), \"b\" to listOf(3))\n\
    val a = m[\"a\"] ?: return \"f1\"\n\
    if (a.sum() != 3) return \"f2\"\n\
    val b = m[\"b\"] ?: return \"f3\"\n\
    if (b.sum() != 3) return \"f4\"\n\
    return \"OK\"\n\
}\n";
    run(src, "NestedGenMap");
}

#[test]
fn generic_constraint_at_call_site() {
    let src = "fun <T : Number> sumAll(xs: List<T>): Double {\n\
    var acc = 0.0\n\
    for (x in xs) acc += x.toDouble()\n\
    return acc\n\
}\n\
fun box(): String {\n\
    if (sumAll(listOf(1, 2, 3)) != 6.0) return \"f1\"\n\
    if (sumAll(listOf(1.5, 2.5)) != 4.0) return \"f2\"\n\
    return \"OK\"\n\
}\n";
    run(src, "GenConstraintCall");
}
