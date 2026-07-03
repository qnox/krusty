//! build.702 aa1 (regression lock): a `suspend` member returning a NULLABLE type, `?: error("…")`-ed to a
//! non-null, then a member accessed on the result — `val c = r.byId(id) ?: error("none"); c.at`. Earlier
//! builds typed the elvis result as `Any` (losing the non-null branch type), so `c.at` failed with
//! "member 'at' on Any". This exercises the faithful shape end-to-end to guard against regression.
use std::path::PathBuf;
mod common;

const LIB: &str = "package lib\n\
    class Card(val at: Int)\n\
    class Repo { suspend fun byId(id: Int): Card? = if (id > 0) Card(id * 2) else null }\n";

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
fn suspend_nullable_return_elvis_error_then_member() {
    const MAIN: &str = "import lib.Repo\n\
        import kotlinx.coroutines.runBlocking\n\
        suspend fun g(r: Repo): Int {\n\
        \x20 val c = r.byId(7) ?: error(\"none\")\n\
        \x20 return c.at\n\
        }\n\
        fun box(): String {\n\
        \x20 val r = runBlocking { g(Repo()) }\n\
        \x20 return if (r == 14) \"OK\" else \"fail: $r\"\n\
        }\n";
    assert_eq!(
        run("aa1", MAIN).expect("suspend nullable + elvis-error + member"),
        "OK"
    );
}
