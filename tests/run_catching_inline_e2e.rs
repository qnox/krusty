//! Exact declaration-owned `runCatching` recovery must survive coroutine lowering. The checked
//! plan carries semantic constructor/failure identities; only the JVM backend realizes `Result`'s
//! value-class carrier.

use super::common;

fn assert_both_compile_and_run(source: &str, stem: &str) {
    let stdlib = common::stdlib_jar();
    let result =
        common::compiler_diagnostics(&[("Main.kt", source)], std::slice::from_ref(&stdlib));
    assert_eq!(
        (result.reference_code, result.reference_stderr.as_str()),
        (0, ""),
        "kotlinc rejected {stem}"
    );
    assert_eq!(
        (result.krusty_code, result.krusty_stderr.as_str()),
        (0, ""),
        "krusty rejected {stem}"
    );
    assert_eq!(common::kotlinc_box_result(source), "OK", "kotlinc {stem}");
    common::expect_box_ok_with_stdlib(source, stem);
}

const SUSPEND_RUNTIME: &str = r#"
import kotlin.coroutines.*

fun <T> runNow(block: suspend () -> T): T {
    var outcome: Result<T>? = null
    block.startCoroutine(object : Continuation<T> {
        override val context: CoroutineContext = EmptyCoroutineContext
        override fun resumeWith(result: Result<T>) { outcome = result }
    })
    return outcome!!.getOrThrow()
}

suspend fun load(value: String): String = suspendCoroutine { continuation ->
    continuation.resume(value.uppercase())
}

suspend fun boom(): String = suspendCoroutine { continuation ->
    continuation.resumeWithException(IllegalStateException("caught"))
}

suspend fun twice(value: Int): Int = suspendCoroutine { continuation ->
    continuation.resume(value * 2)
}

fun box(): String = runNow {
    val success = runCatching { load("ok") }
    if (success.getOrNull() != "OK") return@runNow "success:$success"
    val failure = runCatching { boom() }
    val thrown = failure.exceptionOrNull()
    if (thrown !is IllegalStateException || thrown.message != "caught") {
        return@runNow "failure:$failure"
    }
    val receiver = 21.runCatching { twice(this) }
    if (receiver.getOrNull() != 42) "receiver:$receiver" else "OK"
}
"#;

#[test]
fn suspension_and_both_recovery_arms_match_kotlinc() {
    assert_both_compile_and_run(SUSPEND_RUNTIME, "RunCatchingSuspendRecovery");
}

#[test]
fn minimal_non_suspend_expansion_is_byte_identical() {
    const SOURCE: &str = "fun value(): Result<String> = runCatching { \"OK\" }\n";
    match common::byte_diff_against_kotlinc_cp(
        "RunCatchingMinimalBytes",
        SOURCE,
        "RunCatchingMinimalBytesKt",
        &[common::stdlib_jar()],
    ) {
        None => panic!("RunCatchingMinimalBytes: reference toolchain unavailable"),
        Some(Ok(())) => {}
        Some(Err(error)) => panic!("{error}"),
    }
}
