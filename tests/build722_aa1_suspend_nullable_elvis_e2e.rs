//! build.722 aa1 (regression lock): a `suspend` member returning a NULLABLE type, `?: error("…")`-ed to a
//! non-null, then a member accessed on the result — `val c = r.byId("x") ?: error("nf"); c.at`. Earlier
//! builds typed the elvis result `Any` (losing the non-null branch type) → "member 'at' on Any". This
//! faithful shape (interface `Repo` implemented by a classpath `object`) already compiles and runs; the
//! test guards against regression, and is the root of several further `member … on Any` cascades.
use std::path::PathBuf;
mod common;

const LIB: &str = "package lib\n\
    data class Ch(val at: Int)\n\
    interface Repo { suspend fun byId(id: String): Ch? }\n\
    object R : Repo { override suspend fun byId(id: String): Ch? = if (id == \"x\") Ch(42) else null }\n";

fn run(tag: &str, main: &str) -> Option<String> {
    let jdk = common::jdk_modules()?;
    let sl = common::stdlib_jar()?;
    let coro =
        PathBuf::from("target/cache/kotlinc/2.4.0/kotlinc/lib/kotlinx-coroutines-core-jvm.jar");
    if !coro.exists() {
        return None;
    }
    let lo = common::compile_lib(tag, LIB)?;
    common::compile_and_run_box(main, "Main", &[lo, sl, coro, jdk.clone()], Some(&jdk))
}

#[test]
fn suspend_nullable_elvis_error_then_member() {
    const MAIN: &str = "import lib.*\n\
        import kotlinx.coroutines.runBlocking\n\
        suspend fun g(): Int { val c = R.byId(\"x\") ?: error(\"nf\"); return c.at }\n\
        fun box(): String = runBlocking { if (g() == 42) \"OK\" else \"F\" }\n";
    assert_eq!(
        run("aa1_722", MAIN).expect("suspend nullable elvis-error member"),
        "OK"
    );
}
