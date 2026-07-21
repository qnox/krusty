//! build.840 nn1: `m.withLock { … }` — `kotlinx.coroutines.sync.withLock` is a `suspend inline`
//! EXTENSION on `Mutex` with a DEFAULTED leading `owner: Any? = null` parameter and a trailing
//! `action: () -> T` lambda. The call omits `owner` and passes the lambda. It failed as
//! `unresolved method 'withLock' on Mutex` — an `inline` function has no `$default` synthetic, so the
//! trailing-default extension path missed it, and the whole locked block then typed `Any`
//! (a production service's `member … on Any`). The metadata generic signature
//! drops the synthetic `Continuation`, so the logical shape is `Mutex.withLock(Any?, () -> T): T`;
//! resolution omits the defaulted `owner`, binds `T` from the lambda, and the body is spliced.
use super::common;

fn run(src: &str) -> Option<String> {
    let jdk = common::jdk_modules()?;
    let sl = common::stdlib_jar()?;
    let coro = common::coroutines_jar()?;
    common::compile_and_run_box(src, "Main", &[sl, coro, jdk.clone()], Some(&jdk))
}

// `withLock` is a `suspend inline` EXTENSION on `Mutex` with a defaulted leading `owner: Any? = null`. The
// merged member/extension selector omits the default and binds `T` from the lambda; because withLock also
// emits a real `withLock$default` synthetic (only genuine `@InlineOnly` callees force a splice), it resolves
// to that synthetic — a normal suspend `$default` call — rather than inlining. The CPS `Continuation` is
// emit-only (dropped from the logical params at the metadata boundary, re-inserted before the mask/marker by
// the coroutine pass), and the suspend EXTENSION call marks the enclosing `runBlocking { … }` lambda a
// coroutine state machine. So the block compiles and runs: lock → action → unlock, returning `T`.
#[test]
fn with_lock_omits_default_owner_and_binds_lambda_return() {
    const SRC: &str = "import kotlinx.coroutines.sync.Mutex\n\
        import kotlinx.coroutines.sync.withLock\n\
        import kotlinx.coroutines.runBlocking\n\
        fun box(): String = runBlocking {\n\
        \x20   val m = Mutex()\n\
        \x20   val r = m.withLock { 42 }\n\
        \x20   if (r == 42) \"OK\" else \"F:$r\"\n\
        }\n";
    assert_eq!(
        run(SRC).expect("withLock resolves + compiles + runs"),
        "OK",
        "withLock omits default owner + binds lambda return"
    );
}
