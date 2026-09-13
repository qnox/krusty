//! `Closeable.use { … }` — an inline extension whose body invokes its lambda with the receiver, then
//! calls a TOP-LEVEL cleanup (`closeFinally(resource, cause)`) on every exit with the exception that
//! left the body.
//!
//! krusty expands a recognized inline body at IR level, which is what lets a suspension inside the
//! lambda join the CALLER's state machine. The recogniser understood an enter/lambda/finally shape
//! whose cleanup is a MEMBER on the receiver (`Mutex.unlock`, `Semaphore.release`) and rejected
//! anything else with a call in its body. `use` has no enter call at all, its cleanup is static, and
//! it threads a recorded cause — so it decoded to no plan, the lambda stayed a real function object,
//! and a suspend call inside it never received a continuation. Emission then failed for the whole
//! FILE, losing every class in the module to one `use`.

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

const RESOURCE: &str = "class Probe : java.io.Closeable {\n\
    \x20   var closed = 0\n\
    \x20   override fun close() { closed++ }\n\
    }\n";

/// The failing shape: a SUSPENDING call inside the `use` lambda.
#[test]
fn use_hosts_a_suspension_in_its_lambda() {
    let src = format!(
        "{RESOURCE}import kotlinx.coroutines.runBlocking\n\
        suspend fun load(key: String): String = key.uppercase()\n\
        fun box(): String = runBlocking {{\n\
        \x20   val probe = Probe()\n\
        \x20   val r = probe.use {{ load(\"ok\") }}\n\
        \x20   if (r == \"OK\" && probe.closed == 1) \"OK\" else \"F:$r/${{probe.closed}}\"\n\
        }}\n"
    );
    assert_eq!(
        run(&src).expect("use hosting a suspension compiles and runs"),
        "OK"
    );
}

/// The lambda receives the resource as its argument, not as a receiver — `it` is the `Closeable`.
#[test]
fn use_passes_the_resource_to_its_lambda() {
    let src = format!(
        "{RESOURCE}import kotlinx.coroutines.runBlocking\n\
        suspend fun count(probe: Probe): Int = probe.closed\n\
        fun box(): String = runBlocking {{\n\
        \x20   val probe = Probe()\n\
        \x20   val during = probe.use {{ count(it) }}\n\
        \x20   if (during == 0 && probe.closed == 1) \"OK\" else \"F:$during/${{probe.closed}}\"\n\
        }}\n"
    );
    assert_eq!(
        run(&src).expect("use passes its receiver to the lambda"),
        "OK"
    );
}

/// The resource is closed on the exceptional path too. This is what proves the expansion kept the
/// `finally`, not merely that the call type-checked.
#[test]
fn use_closes_on_the_exceptional_path() {
    let src = format!(
        "{RESOURCE}import kotlinx.coroutines.runBlocking\n\
        suspend fun boom(): String = throw IllegalStateException(\"x\")\n\
        fun box(): String = runBlocking {{\n\
        \x20   val probe = Probe()\n\
        \x20   try {{\n\
        \x20       probe.use {{ boom() }}\n\
        \x20   }} catch (e: IllegalStateException) {{\n\
        \x20   }}\n\
        \x20   if (probe.closed == 1) \"OK\" else \"F:${{probe.closed}}\"\n\
        }}\n"
    );
    assert_eq!(
        run(&src).expect("use closes its resource when the body throws"),
        "OK"
    );
}

/// A failure from `close()` is SUPPRESSED onto the body's exception rather than replacing it. That
/// is the whole point of the recorded cause the cleanup receives, so the expansion must thread it.
#[test]
fn use_suppresses_a_close_failure_onto_the_body_exception() {
    const SRC: &str = "import kotlinx.coroutines.runBlocking\n\
        class Rude : java.io.Closeable {\n\
        \x20   override fun close() { throw IllegalArgumentException(\"close\") }\n\
        }\n\
        suspend fun boom(): String = throw IllegalStateException(\"body\")\n\
        fun box(): String = runBlocking {\n\
        \x20   try {\n\
        \x20       Rude().use { boom() }\n\
        \x20       \"F:no throw\"\n\
        \x20   } catch (e: Throwable) {\n\
        \x20       val suppressed = e.suppressed.map { it.message }\n\
        \x20       if (e.message == \"body\" && suppressed == listOf(\"close\")) \"OK\"\n\
        \x20       else \"F:${e.message}/$suppressed\"\n\
        \x20   }\n\
        }\n";
    assert_eq!(
        run(SRC).expect("use suppresses a close failure onto the body exception"),
        "OK"
    );
}
