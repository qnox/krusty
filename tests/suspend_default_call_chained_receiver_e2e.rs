use super::common;

const LIB: &str = "package lib\n\
    interface R { suspend fun all(k: String): List<String> }\n\
    object Impl : R { override suspend fun all(k: String): List<String> = listOf(k, k + \"x\") }\n";

fn run(tag: &str, main: &str) -> Option<String> {
    let jdk = common::jdk_modules()?;
    let sl = common::stdlib_jar()?;
    let coro = common::coroutines_jar()?;
    let lo = common::compile_lib(tag, LIB)?;
    common::compile_and_run_box(main, "Main", &[lo, sl, coro, jdk.clone()], Some(&jdk))
}

#[test]
fn chained_lambda_call_on_suspend_default_result() {
    const MAIN: &str = "import lib.*\n\
        import kotlinx.coroutines.runBlocking\n\
        class S(val r: R) {\n\
            suspend fun g(k: String): String = f(k).joinToString(\",\")\n\
            private suspend fun f(k: String, o: String = \"D\"): List<String> = r.all(k).map { it + o }\n\
        }\n\
        fun box(): String = runBlocking { S(Impl).g(\"a\") }\n";
    assert_eq!(
        run("susp_def_chain", MAIN).expect("chained lambda"),
        "aD,axD"
    );
}

#[test]
fn chained_plain_call_on_suspend_default_result() {
    const MAIN: &str = "import lib.*\n\
        import kotlinx.coroutines.runBlocking\n\
        class S(val r: R) {\n\
            suspend fun g(k: String): String = f(k).first()\n\
            private suspend fun f(k: String, o: String = \"D\"): List<String> = r.all(k).map { it + o }\n\
        }\n\
        fun box(): String = runBlocking { S(Impl).g(\"a\") }\n";
    assert_eq!(
        run("susp_def_plain", MAIN).expect("chained plain call"),
        "aD"
    );
}

#[test]
fn block_body_return_of_chained_call_in_concat() {
    const MAIN: &str = "import lib.*\n\
        import kotlinx.coroutines.runBlocking\n\
        class S(val r: R) {\n\
            suspend fun g(k: String): String {\n\
                return f(k).joinToString(\",\") + \"|\"\n\
            }\n\
            private suspend fun f(k: String, o: String = \"D\"): List<String> = r.all(k).map { it + o }\n\
        }\n\
        fun box(): String = runBlocking { S(Impl).g(\"a\") }\n";
    assert_eq!(run("susp_def_block", MAIN).expect("block body"), "aD,axD|");
}

#[test]
fn chained_call_with_explicit_argument() {
    const MAIN: &str = "import lib.*\n\
        import kotlinx.coroutines.runBlocking\n\
        class S(val r: R) {\n\
            suspend fun g(k: String): String = f(k, \"E\").joinToString(\",\")\n\
            private suspend fun f(k: String, o: String = \"D\"): List<String> = r.all(k).map { it + o }\n\
        }\n\
        fun box(): String = runBlocking { S(Impl).g(\"a\") }\n";
    assert_eq!(run("susp_def_expl", MAIN).expect("explicit arg"), "aE,axE");
}

#[test]
fn local_bound_suspend_default_still_works() {
    const MAIN: &str = "import lib.*\n\
        import kotlinx.coroutines.runBlocking\n\
        class S(val r: R) {\n\
            suspend fun g(k: String): String {\n\
                val xs = f(k)\n\
                return xs.joinToString(\",\")\n\
            }\n\
            private suspend fun f(k: String, o: String = \"D\"): List<String> = r.all(k).map { it + o }\n\
        }\n\
        fun box(): String = runBlocking { S(Impl).g(\"a\") }\n";
    assert_eq!(run("susp_def_local", MAIN).expect("local bound"), "aD,axD");
}
