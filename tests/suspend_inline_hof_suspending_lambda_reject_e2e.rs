use super::common;

fn run(main: &str) -> Option<String> {
    let jdk = common::jdk_modules()?;
    let sl = common::stdlib_jar()?;
    let coro = common::coroutines_jar()?;
    common::compile_and_run_box(main, "Main", &[sl, coro, jdk.clone()], Some(&jdk))
}

#[test]
fn withlock_lambda_with_single_suspend_call_runs() {
    const MAIN: &str = "import kotlinx.coroutines.sync.Mutex\n\
        import kotlinx.coroutines.sync.withLock\n\
        import kotlinx.coroutines.runBlocking\n\
        suspend fun make(): String = \"x\"\n\
        suspend fun f(m: Mutex): String = m.withLock { make() }\n\
        fun box(): String = runBlocking { f(Mutex()) }\n";
    assert_eq!(run(MAIN).expect("withLock single suspend"), "x");
}

#[test]
fn withlock_lambda_calling_suspending_member_runs() {
    const MAIN: &str = "import kotlinx.coroutines.sync.Mutex\n\
        import kotlinx.coroutines.sync.withLock\n\
        import kotlinx.coroutines.runBlocking\n\
        class C(val base: Int) {\n\
            private val m = Mutex()\n\
            suspend fun scaled(): Int = m.withLock { step(base) }\n\
            private suspend fun step(v: Int): Int = v * 10\n\
        }\n\
        fun box(): String = runBlocking { C(2).scaled().toString() }\n";
    assert_eq!(run(MAIN).expect("withLock member"), "20");
}

#[test]
fn withlock_lambda_without_suspension_still_runs() {
    const MAIN: &str = "import kotlinx.coroutines.sync.Mutex\n\
        import kotlinx.coroutines.sync.withLock\n\
        import kotlinx.coroutines.runBlocking\n\
        fun box(): String = runBlocking { val m = Mutex(); val r = m.withLock { 42 }; if (r == 42) \"OK\" else \"F\" }\n";
    assert_eq!(run(MAIN).expect("withLock no suspension"), "OK");
}

#[test]
fn withlock_lambda_with_suspend_and_nonlocal_return_compiles() {
    let (Some(stdlib), Some(coro), Some(jdk)) = (
        common::stdlib_jar(),
        common::coroutines_jar(),
        common::jdk_modules(),
    ) else {
        return;
    };
    const SRC: &str = "import kotlinx.coroutines.sync.Mutex\n\
         import kotlinx.coroutines.sync.withLock\n\
         suspend fun make(): String = \"x\"\n\
         suspend fun f(m: Mutex, s: String?): String = m.withLock { s?.let { return@withLock it }; make() }\n";
    let rejected = common::backend_rejects_in_process(SRC, "S", &[stdlib, coro], Some(&jdk));
    assert_eq!(
        rejected,
        Some(false),
        "the non-local-return withLock shape should compile, not be declined"
    );
}
