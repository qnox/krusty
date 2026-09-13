//! `Semaphore.withPermit { … }` — a `suspend inline` extension whose enter/cleanup members take NO
//! argument besides the receiver.
//!
//! krusty expands a recognized inline body at IR level, which is what lets a suspension inside the
//! lambda join the CALLER's state machine. The recogniser for the enter/lambda/finally shape read
//! the enter call's receiver from a fixed operand position, which assumed exactly one state
//! argument: `Mutex.withLock` passes its `owner` to `lock`/`unlock`, so it decoded, while
//! `Semaphore.withPermit` passes nothing to `acquire`/`release` and did not.
//!
//! Without a plan the call kept its lambda as a real function object, a suspend call inside it never
//! received a continuation, and emission failed with "call arity mismatch" — which bails the whole
//! FILE, so every class in the module was lost to one `withPermit`.

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

/// The failing shape: a SUSPENDING call inside the `withPermit` lambda.
#[test]
fn with_permit_hosts_a_suspension_in_its_lambda() {
    const SRC: &str = "import kotlinx.coroutines.sync.Semaphore\n\
        import kotlinx.coroutines.sync.withPermit\n\
        import kotlinx.coroutines.runBlocking\n\
        suspend fun load(key: String): String = key.uppercase()\n\
        fun box(): String = runBlocking {\n\
        \x20   val gate = Semaphore(1)\n\
        \x20   val r = gate.withPermit { load(\"ok\") }\n\
        \x20   if (r == \"OK\") \"OK\" else \"F:$r\"\n\
        }\n";
    assert_eq!(
        run(SRC).expect("withPermit hosting a suspension compiles and runs"),
        "OK"
    );
}

/// Both member calls in the structural plan return Kotlin `Unit`. Their physical suspend/JVM
/// descriptors use `Object`/`void`, so this catches any attempt to reconstruct the semantic result
/// from those descriptors instead of the declarations' metadata.
#[test]
fn with_permit_preserves_a_typed_unit_result() {
    const SRC: &str = "import kotlinx.coroutines.sync.Semaphore\n\
        import kotlinx.coroutines.sync.withPermit\n\
        import kotlinx.coroutines.runBlocking\n\
        suspend fun tick(): Unit = Unit\n\
        suspend fun guarded(gate: Semaphore): Unit = gate.withPermit { tick() }\n\
        fun box(): String = runBlocking {\n\
        \x20   val gate = Semaphore(1)\n\
        \x20   val result: Unit = guarded(gate)\n\
        \x20   if (result == Unit && gate.availablePermits == 1) \"OK\" else \"FAIL\"\n\
        }\n";
    assert_eq!(
        run(SRC).expect("withPermit keeps its metadata-declared Unit member results"),
        "OK"
    );
}

/// The permit is released on the exceptional path too, so the second acquire still succeeds. This is
/// what proves the expansion kept the `finally`, not merely that the call type-checked.
#[test]
fn with_permit_releases_on_the_exceptional_path() {
    const SRC: &str = "import kotlinx.coroutines.sync.Semaphore\n\
        import kotlinx.coroutines.sync.withPermit\n\
        import kotlinx.coroutines.runBlocking\n\
        suspend fun boom(): String = throw IllegalStateException(\"x\")\n\
        fun box(): String = runBlocking {\n\
        \x20   val gate = Semaphore(1)\n\
        \x20   try {\n\
        \x20       gate.withPermit { boom() }\n\
        \x20   } catch (e: IllegalStateException) {\n\
        \x20   }\n\
        \x20   val after = gate.withPermit { \"released\" }\n\
        \x20   if (after == \"released\" && gate.availablePermits == 1) \"OK\" else \"F:$after\"\n\
        }\n";
    assert_eq!(
        run(SRC).expect("withPermit releases its permit when the body throws"),
        "OK"
    );
}

/// Control: `withLock`, which DOES thread a state argument, keeps working — the recogniser must read
/// the enter member's descriptor, not simply drop the state argument.
#[test]
fn with_lock_still_threads_its_owner() {
    const SRC: &str = "import kotlinx.coroutines.sync.Mutex\n\
        import kotlinx.coroutines.sync.withLock\n\
        import kotlinx.coroutines.runBlocking\n\
        suspend fun load(key: String): String = key.uppercase()\n\
        fun box(): String = runBlocking {\n\
        \x20   val m = Mutex()\n\
        \x20   val r = m.withLock { load(\"ok\") }\n\
        \x20   if (r == \"OK\" && !m.isLocked) \"OK\" else \"F:$r\"\n\
        }\n";
    assert_eq!(
        run(SRC).expect("withLock hosting a suspension compiles and runs"),
        "OK"
    );
}
