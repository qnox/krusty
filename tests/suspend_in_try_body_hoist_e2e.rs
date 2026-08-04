use super::common;

const LIB: &str = "package lib\n\
    interface R {\n\
        suspend fun all(k: String): List<String>\n\
        suspend fun boom(k: String): String\n\
    }\n\
    object Impl : R {\n\
        override suspend fun all(k: String): List<String> = listOf(k, k + \"x\")\n\
        override suspend fun boom(k: String): String = throw IllegalStateException(\"boom-\" + k)\n\
    }\n";

fn run(tag: &str, main: &str) -> Option<String> {
    let jdk = common::jdk_modules();
    let sl = common::stdlib_jar();
    let coro = common::coroutines_jar();
    let lo = common::compile_lib(tag, LIB)?;
    common::compile_and_run_box(
        main,
        "Main",
        &[lo, sl, coro, jdk.clone()],
        Some(jdk.as_path()),
    )
}

#[test]
fn suspension_chained_inside_inlined_withlock() {
    const MAIN: &str = "import lib.*\n\
        import kotlinx.coroutines.runBlocking\n\
        import kotlinx.coroutines.sync.Mutex\n\
        import kotlinx.coroutines.sync.withLock\n\
        class S(val r: R) {\n\
            private val m = Mutex()\n\
            suspend fun g(k: String): String = m.withLock { r.all(k).first() }\n\
        }\n\
        fun box(): String = runBlocking { S(Impl).g(\"a\") }\n";
    assert_eq!(run("susp_try_lock", MAIN).expect("withLock chained"), "a");
}

#[test]
fn suspension_chained_inside_explicit_try_finally() {
    const MAIN: &str = "import lib.*\n\
        import kotlinx.coroutines.runBlocking\n\
        class S(val r: R) {\n\
            var log = \"\"\n\
            suspend fun g(k: String): String {\n\
                var v = \"\"\n\
                try {\n\
                    v = r.all(k).first() + \"!\"\n\
                } finally {\n\
                    log += \"F\"\n\
                }\n\
                return v\n\
            }\n\
        }\n\
        fun box(): String = runBlocking {\n\
            val s = S(Impl)\n\
            val v = s.g(\"a\")\n\
            v + \"/\" + s.log\n\
        }\n";
    assert_eq!(run("susp_try_fin", MAIN).expect("try/finally"), "a!/F");
}

#[test]
fn throws_inside_try_still_caught() {
    const MAIN: &str = "import lib.*\n\
        import kotlinx.coroutines.runBlocking\n\
        class S(val r: R) {\n\
            suspend fun g(k: String): String {\n\
                try {\n\
                    return r.boom(k).uppercase()\n\
                } catch (e: IllegalStateException) {\n\
                    return \"caught:\" + e.message\n\
                }\n\
            }\n\
        }\n\
        fun box(): String = runBlocking { S(Impl).g(\"a\") }\n";
    assert_eq!(
        run("susp_try_catch", MAIN).expect("throw inside try"),
        "caught:boom-a"
    );
}

#[test]
fn suspension_chained_inside_catch_body() {
    const MAIN: &str = "import lib.*\n\
        import kotlinx.coroutines.runBlocking\n\
        class S(val r: R) {\n\
            suspend fun g(k: String): String {\n\
                try {\n\
                    return r.boom(k)\n\
                } catch (e: IllegalStateException) {\n\
                    return r.all(k).last()\n\
                }\n\
            }\n\
        }\n\
        fun box(): String = runBlocking { S(Impl).g(\"a\") }\n";
    assert_eq!(run("susp_catch_body", MAIN).expect("catch body"), "ax");
}
