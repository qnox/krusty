//! `list.map { … }` / `list.flatMap { … }` whose LAMBDA BODY calls a suspend function. A stdlib
//! collection HOF lowers its lambda to a `FunctionN` impl that cannot suspend, so krusty inlines it into
//! an accumulating loop (kotlinc's own inline expansion) — the suspension then lives in an ordinary
//! for-loop the coroutine pass models. Also covers `list.addAll(source.items())` inside a `for` (a
//! suspend call buried in a call argument, hoisted to a temp). The synthetic fixtures exercise both
//! `sources.flatMap { it.items() }` and nested `for { values.addAll(source.items()) }` shapes without
//! retaining names from an originating application.
//! Needs the JVM toolchain + kotlin-stdlib + coroutines + real kotlinc; skips otherwise.
use super::common;

const LIB: &str = "package lib\n\
    interface Src { suspend fun items(): List<Int> }\n\
    class Impl(val xs: List<Int>) : Src { override suspend fun items(): List<Int> = xs }\n\
    interface Xf { suspend fun apply(x: Int): Int }\n\
    class Inc : Xf { override suspend fun apply(x: Int): Int = x + 1 }\n";

#[test]
fn suspend_lambda_in_collection_hof_runs() {
    let jdk = common::jdk_modules();
    let sl = common::stdlib_jar();
    let coro = common::coroutines_jar();
    let Some(lo) = common::compile_lib("susp_hof_lambda", LIB) else {
        return;
    };
    const MAIN: &str = "import lib.*\n\
        import kotlinx.coroutines.runBlocking\n\
        suspend fun flat(ss: List<Src>): List<Int> = ss.flatMap { it.items() }\n\
        suspend fun sizes(ss: List<Src>): List<Int> = ss.map { it.items().size }\n\
        suspend fun viaAddAll(ss: List<Src>): List<Int> {\n\
            val acc = mutableListOf<Int>()\n\
            for (s in ss) { acc.addAll(s.items()) }\n\
            return acc\n\
        }\n\
        suspend fun incAll(xs: List<Int>, xf: Xf): List<Int> = xs.map { xf.apply(it) }\n\
        fun box(): String = runBlocking {\n\
            val ss = listOf(Impl(listOf(1, 2)), Impl(listOf(3)))\n\
            val f = flat(ss); val m = sizes(ss); val a = viaAddAll(ss)\n\
            val p = incAll(listOf(10, 20), Inc())\n\
            if (f == listOf(1, 2, 3) && m == listOf(2, 1) && a == listOf(1, 2, 3) && p == listOf(11, 21)) \"OK\"\n\
            else \"F f=$f m=$m a=$a p=$p\"\n\
        }\n";
    let out = common::compile_and_run_box(
        MAIN,
        "Main",
        &[lo, sl, coro, jdk.clone()],
        Some(jdk.as_path()),
    );
    assert_eq!(
        out.as_deref(),
        Some("OK"),
        "suspend lambda in collection HOF"
    );
}

#[test]
fn suspend_map_hof_uses_declaration_iterator_scope() {
    let jdk = common::jdk_modules();
    let stdlib = common::stdlib_jar();
    let coroutines = common::coroutines_jar();
    const SOURCE: &str = "import kotlinx.coroutines.runBlocking\n\
        operator fun <K, V> Map<K, V>.iterator(): Iterator<Map.Entry<K, V>> =\n\
            emptyList<Map.Entry<K, V>>().iterator()\n\
        suspend fun render(entry: Map.Entry<String, Int>): String = entry.key\n\
        suspend fun collect(values: Map<String, Int>): List<String> =\n\
            values.map { render(it) }\n\
        fun box(): String = runBlocking {\n\
            collect(mapOf(\"O\" to 1, \"K\" to 2)).joinToString(\"\")\n\
        }\n";
    let output = common::compile_and_run_box(
        SOURCE,
        "Main",
        &[stdlib, coroutines, jdk.clone()],
        Some(jdk.as_path()),
    );
    assert_eq!(output.as_deref(), Some("OK"));
}
