//! build.840 nn1: `m.withLock { … }` — `kotlinx.coroutines.sync.withLock` is a `suspend inline`
//! EXTENSION on `Mutex` with a DEFAULTED leading `owner: Any? = null` parameter and a trailing
//! `action: () -> T` lambda. The call omits `owner` and passes the lambda. It failed as
//! `unresolved method 'withLock' on Mutex` — an `inline` function has no `$default` synthetic, so the
//! trailing-default extension path missed it, and the whole locked block then typed `Any`
//! (`WorkspaceService`/`MissionDriftService`'s `member … on Any`). The metadata generic signature
//! drops the synthetic `Continuation`, so the logical shape is `Mutex.withLock(Any?, () -> T): T`;
//! resolution omits the defaulted `owner`, binds `T` from the lambda, and the body is spliced.
use super::common;

fn run(src: &str) -> Option<String> {
    let jdk = common::jdk_modules()?;
    let sl = common::stdlib_jar()?;
    let coro = common::coroutines_jar()?;
    common::compile_and_run_box(src, "Main", &[sl, coro, jdk.clone()], Some(&jdk))
}

// RESOLUTION + suspend-context detection now work: `withLock` resolves as a defaulted inline extension
// (the merged member/extension selector omits the `owner` default and binds `T` from the lambda), and the
// enclosing `runBlocking { … }` lambda is recognized as a coroutine state machine (a suspend EXTENSION call
// is no longer invisible to the lowerer's suspension detection). The remaining gap is EMIT: `withLock` is a
// `suspend inline` function, so kotlinc materializes it by INLINING its body — whose inner `lock`/`unlock`
// suspensions must be threaded into the enclosing state machine. krusty builds state machines from
// IR-level `suspend_calls`, but a MUST-INLINE splice hides those suspensions from the IR, so the SM builder
// bails ("no suspend call in any stmt"). Closing this needs suspend-inline-body splicing with continuation
// threading — a coroutine-codegen feature tracked separately. Kept as the red TDD target for that work.
#[ignore = "suspend inline-extension splice with continuation threading (withLock body) — pending coroutine codegen"]
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
