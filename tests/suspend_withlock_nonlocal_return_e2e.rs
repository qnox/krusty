//! A `mutex.withLock { … }` (a `suspend inline fun`) whose lambda body SUSPENDS and returns non-locally
//! via `return@withLock` — including two returns buried inside nested `?.let { … }` scope-lambdas and a
//! final `return@withLock <suspending call>`. The coroutine pass inlines the lock/try-finally/unlock,
//! flattens the suspending body across states, routes each labeled break (even the ones nested inside the
//! non-suspending `?.let` expansions) to a state transition, and stores each returned value into the
//! withLock result slot. Needs the JVM toolchain + kotlin-stdlib + coroutines + real kotlinc.
use super::common;

#[test]
fn suspend_withlock_nonlocal_return_runs() {
    let Some(jdk) = common::jdk_modules() else {
        return;
    };
    let Some(sl) = common::stdlib_jar() else {
        return;
    };
    let Some(coro) = common::coroutines_jar() else {
        return;
    };
    // getOrCreate exercises all three `return@withLock` paths across calls with different repo state:
    //   r1 empty:          first()/second() null -> `return@withLock create(key)` (SUSPENDS in the tail)
    //   r1 after create:   first() non-null      -> first nested `?.let` returns non-locally
    //   r2 (only second):  first() null, second() non-null -> second nested `?.let` returns non-locally
    const MAIN: &str = "import kotlinx.coroutines.runBlocking\n\
        import kotlinx.coroutines.sync.Mutex\n\
        import kotlinx.coroutines.sync.withLock\n\
        class Repo(var a: String?, var b: String?) {\n\
            suspend fun first(): String? = a\n\
            suspend fun second(): String? = b\n\
            suspend fun create(x: String): String { a = x; return x }\n\
        }\n\
        val mutex = Mutex()\n\
        suspend fun getOrCreate(repo: Repo, key: String): String = mutex.withLock {\n\
            repo.first()?.let { return@withLock it }\n\
            repo.second()?.let { return@withLock it }\n\
            return@withLock repo.create(key)\n\
        }\n\
        fun box(): String = runBlocking {\n\
            val r1 = Repo(null, null)\n\
            val created = getOrCreate(r1, \"x\")\n\
            val fromFirst = getOrCreate(r1, \"y\")\n\
            val r2 = Repo(null, \"z\")\n\
            val fromSecond = getOrCreate(r2, \"w\")\n\
            if (created == \"x\" && fromFirst == \"x\" && fromSecond == \"z\") \"OK\"\n\
            else \"F created=$created fromFirst=$fromFirst fromSecond=$fromSecond\"\n\
        }\n";
    let out = common::compile_and_run_box(MAIN, "Main", &[sl, coro, jdk.clone()], Some(&jdk));
    assert_eq!(
        out.as_deref(),
        Some("OK"),
        "withLock body must store each `return@withLock` value (nested `?.let` and suspending tail) and \
         run under the coroutine state machine"
    );
}
