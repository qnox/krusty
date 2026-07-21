//! `list.map { … }` / `list.flatMap { … }` whose LAMBDA BODY calls a suspend function. A stdlib
//! collection HOF lowers its lambda to a `FunctionN` impl that cannot suspend, so krusty inlines it into
//! an accumulating loop (kotlinc's own inline expansion) — the suspension then lives in an ordinary
//! for-loop the coroutine pass models. Also covers `list.addAll(repo.get())` inside a `for` (a suspend
//! call buried in a call argument, hoisted to a temp). Production hit: a deployment-options
//! service (`getServices<T>(id).flatMap { it.getDeployables(session) }`,
//! nested `for { options.addAll(deployer.getDeploymentOptions(x)) }`).
//! Needs the JVM toolchain + kotlin-stdlib + coroutines + real kotlinc; skips otherwise.
use super::common;

const LIB: &str = "package lib\n\
    interface Src { suspend fun items(): List<Int> }\n\
    class Impl(val xs: List<Int>) : Src { override suspend fun items(): List<Int> = xs }\n\
    interface Xf { suspend fun apply(x: Int): Int }\n\
    class Inc : Xf { override suspend fun apply(x: Int): Int = x + 1 }\n";

#[test]
fn suspend_lambda_in_collection_hof_runs() {
    let Some(jdk) = common::jdk_modules() else {
        return;
    };
    let Some(sl) = common::stdlib_jar() else {
        return;
    };
    let Some(coro) = common::coroutines_jar() else {
        return;
    };
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
    let out = common::compile_and_run_box(MAIN, "Main", &[lo, sl, coro, jdk.clone()], Some(&jdk));
    assert_eq!(
        out.as_deref(),
        Some("OK"),
        "suspend lambda in collection HOF"
    );
}
