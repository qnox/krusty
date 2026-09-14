//! `runCatching { … }` — a guarded invocation whose ARMS are call chains: the normal exit wraps the
//! lambda's result, and the exceptional exit builds a value from the throwable that left it.
//!
//! krusty expands a recognized classpath inline body at IR level, which is what lets a suspension
//! inside the lambda join the CALLER's state machine. The recogniser read every call's operands by
//! scanning backwards for LOCAL loads, so it could only see values that pass through a local.
//! `runCatching` hands the invocation's result straight to its wrapper on the JVM stack and never
//! stores it, so the body decoded to nothing at all.
//!
//! Without a plan the call keeps its lambda as a real function object, a suspend call inside it
//! never receives a continuation, and emission fails with "call arity mismatch" — which bails the
//! whole FILE, so one `runCatching` costs a module every class it would have emitted.

use super::common;

fn run(src: &str) -> Option<String> {
    let jdk = common::jdk_modules();
    let stdlib = common::stdlib_jar();
    let coroutines = common::coroutines_jar();
    common::compile_and_run_box(
        src,
        "Main",
        &[stdlib, coroutines, jdk.clone()],
        Some(jdk.as_path()),
    )
}

/// The failing shape: a SUSPENDING call inside the `runCatching` lambda.
#[test]
fn run_catching_hosts_a_suspension_in_its_lambda() {
    const SRC: &str = "import kotlinx.coroutines.runBlocking\n\
        suspend fun load(key: String): String = key.uppercase()\n\
        fun box(): String = runBlocking {\n\
        \x20   val r = runCatching { load(\"ok\") }.getOrNull()\n\
        \x20   if (r == \"OK\") \"OK\" else \"F:$r\"\n\
        }\n";
    assert_eq!(
        run(SRC).expect("runCatching hosting a suspension compiles and runs"),
        "OK"
    );
}

/// The exceptional arm captures the throwable rather than letting it escape, and reports it as a
/// failure carrying that exact exception.
#[test]
fn run_catching_captures_a_throwing_suspension_as_a_failure() {
    const SRC: &str = "import kotlinx.coroutines.runBlocking\n\
        suspend fun boom(): String = throw IllegalStateException(\"x\")\n\
        fun box(): String = runBlocking {\n\
        \x20   val result = runCatching { boom() }\n\
        \x20   val thrown = result.exceptionOrNull()\n\
        \x20   if (result.isFailure && thrown is IllegalStateException && thrown.message == \"x\") \"OK\"\n\
        \x20   else \"F:$thrown\"\n\
        }\n";
    assert_eq!(
        run(SRC).expect("runCatching reports a throwing body as a failure"),
        "OK"
    );
}

/// The receiver overload passes its receiver to the lambda as that lambda's RECEIVER — the
/// declaration takes a `T.() -> R`, so the body reads it as `this`, not as `it`.
#[test]
fn run_catching_on_a_receiver_passes_it_to_the_lambda() {
    const SRC: &str = "import kotlinx.coroutines.runBlocking\n\
        suspend fun twice(value: Int): Int = value * 2\n\
        fun box(): String = runBlocking {\n\
        \x20   val r = 21.runCatching { twice(this) }.getOrNull()\n\
        \x20   if (r == 42) \"OK\" else \"F:$r\"\n\
        }\n";
    assert_eq!(
        run(SRC).expect("the receiver overload passes its receiver to the lambda"),
        "OK"
    );
}

/// Control: a non-suspending `runCatching` keeps its ordinary success value, so the expansion must
/// not change what a plain call already produced.
#[test]
fn run_catching_without_a_suspension_still_succeeds() {
    const SRC: &str = "fun box(): String {\n\
        \x20   val r = runCatching { \"value\" }\n\
        \x20   return if (r.isSuccess && r.getOrNull() == \"value\") \"OK\" else \"F:$r\"\n\
        }\n";
    assert_eq!(
        run(SRC).expect("a non-suspending runCatching still succeeds"),
        "OK"
    );
}
