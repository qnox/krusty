//! Exact checked expansion of `Closeable.use { ... }` across suspension and both exit paths.

use super::common;

fn run(src: &str) -> String {
    let jdk = common::jdk_modules();
    let stdlib = common::stdlib_jar();
    let coroutines = common::coroutines_jar();
    common::expect_box_run(
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
    assert_eq!(run(&src), "OK");
}

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
    assert_eq!(run(&src), "OK");
}

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
    assert_eq!(run(&src), "OK");
}

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
    assert_eq!(run(SRC), "OK");
}
