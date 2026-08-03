//! A suspend call in the trailing value of a statement-less block produced by inline splicing must be
//! hoisted into the coroutine prelude. The inline wrapper body reduces to a value-position block whose
//! only value is the suspend call; leaving that block opaque made the suspend flattener miss the call.

use super::common;

#[test]
fn inline_wrapper_tail_suspend_call_runs() {
    let jdk = common::jdk_modules();
    let sl = common::stdlib_jar();
    let coro = common::coroutines_jar();
    const MAIN: &str = "import kotlinx.coroutines.runBlocking\n\
        inline fun <T> wrap(block: () -> T): T = block()\n\
        suspend fun one(): Int = 41\n\
        suspend fun f(): Int = wrap { one() }\n\
        fun box(): String = runBlocking {\n\
            val n = f()\n\
            if (n == 41) \"OK\" else \"F:$n\"\n\
        }\n";
    let out =
        common::compile_and_run_box(MAIN, "Main", &[sl, coro, jdk.clone()], Some(jdk.as_path()));
    assert_eq!(
        out.as_deref(),
        Some("OK"),
        "inline wrapper with statement-less suspend tail block"
    );
}
