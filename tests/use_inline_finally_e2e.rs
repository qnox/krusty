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

fn suspending_use_source() -> String {
    format!(
        "import kotlinx.coroutines.runBlocking\n\
        {RESOURCE}\
        suspend fun load(key: String): String = key.uppercase()\n\
        fun box(): String = runBlocking {{\n\
        \x20   val probe = Probe()\n\
        \x20   val r = probe.use {{ load(\"ok\") }}\n\
        \x20   if (r == \"OK\" && probe.closed == 1) \"OK\" else \"F:$r/${{probe.closed}}\"\n\
        }}\n"
    )
}

#[test]
fn use_hosts_a_suspension_in_its_lambda() {
    let src = suspending_use_source();
    assert_eq!(run(&src), "OK");
}

#[test]
fn kotlinc_and_krusty_accept_the_suspend_use_fixture() {
    let source_text = suspending_use_source();
    let jdk = common::jdk_modules();
    let stdlib = common::stdlib_jar();
    let coroutines = common::coroutines_jar();
    let reference = common::scratch_dir().expect("scratch directory");
    let source = reference.join("SuspendInlineUse.kt");
    let output = reference.join("reference");
    std::fs::write(&source, &source_text).expect("write reference source");
    std::fs::create_dir_all(&output).expect("create reference output directory");
    let classpath = std::env::join_paths([stdlib.as_path(), coroutines.as_path()])
        .expect("reference classpath")
        .to_string_lossy()
        .into_owned();
    let (code, stderr) = common::kotlinc_compile(&[
        "-d".to_string(),
        output.to_string_lossy().into_owned(),
        "-cp".to_string(),
        classpath,
        source.to_string_lossy().into_owned(),
    ])
    .expect("reference kotlinc available");
    assert_eq!(code, 0, "kotlinc must accept the runtime fixture: {stderr}");
    assert!(
        common::compile_in_process(
            &source_text,
            "SuspendInlineUse",
            &[stdlib, coroutines],
            Some(jdk.as_path()),
        )
        .is_some(),
        "krusty must accept the same runtime fixture as kotlinc",
    );
    let _ = std::fs::remove_dir_all(reference);
}

#[test]
fn ordinary_use_expands_without_a_coroutine_host() {
    let src = format!(
        "{RESOURCE}fun box(): String {{\n\
        \x20   val probe = Probe()\n\
        \x20   val value = probe.use {{ \"body\" }}\n\
        \x20   return if (value == \"body\" && probe.closed == 1) \"OK\"\n\
        \x20   else \"F:$value/${{probe.closed}}\"\n\
        }}\n"
    );
    assert_eq!(run(&src), "OK");
}

#[test]
fn nullable_use_invokes_the_body_and_skips_null_cleanup() {
    const SRC: &str = "fun box(): String {\n\
        \x20   val resource: java.io.Closeable? = null\n\
        \x20   var called = false\n\
        \x20   val value = resource.use { called = true; if (it == null) \"body\" else \"bad\" }\n\
        \x20   return if (called && value == \"body\") \"OK\" else \"F:$called/$value\"\n\
        }\n";
    assert_eq!(run(SRC), "OK");
}

#[test]
fn a_close_failure_after_a_normal_body_is_propagated() {
    const SRC: &str = "class Rude : java.io.Closeable {\n\
        \x20   override fun close() { throw IllegalArgumentException(\"close\") }\n\
        }\n\
        fun box(): String = try {\n\
        \x20   Rude().use { \"body\" }\n\
        \x20   \"F:no throw\"\n\
        } catch (e: IllegalArgumentException) {\n\
        \x20   if (e.message == \"close\") \"OK\" else \"F:${e.message}\"\n\
        }\n";
    assert_eq!(run(SRC), "OK");
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
