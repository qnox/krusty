//! build.775 ii1: a `suspend` call inside a `for` loop body. `while` loops already lower through the
//! coroutine state machine, but a `for (x in xs) { r.del(x) }` with a suspend call in the body bailed
//! with "this suspend-function shape is not yet supported by the IR backend". Real hit:
//! a production offboarding service method.
use super::common;

const LIB: &str = "package lib\n\
    interface Repo { suspend fun del(x: Int) }\n\
    object R : Repo {\n\
        var total: Int = 0\n\
        override suspend fun del(x: Int) { total += x }\n\
    }\n";

fn run(tag: &str, main: &str) -> Option<String> {
    let jdk = common::jdk_modules()?;
    let sl = common::stdlib_jar()?;
    let coro = common::coroutines_jar()?;
    let lo = common::compile_lib(tag, LIB)?;
    common::compile_and_run_box(main, "Main", &[lo, sl, coro, jdk.clone()], Some(&jdk))
}

#[test]
fn suspend_call_in_for_loop() {
    const MAIN: &str = "import lib.*\n\
        import kotlinx.coroutines.runBlocking\n\
        suspend fun f(r: Repo, xs: List<Int>) { for (x in xs) { r.del(x) } }\n\
        fun box(): String = runBlocking { f(R, listOf(1, 2, 3)); if (R.total == 6) \"OK\" else \"F:\" + R.total }\n";
    assert_eq!(
        run("ii1_775", MAIN).expect("suspend call in for loop"),
        "OK"
    );
}
